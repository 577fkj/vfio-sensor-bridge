// SPDX-License-Identifier: GPL-2.0-or-later
#include <linux/capability.h>
#include <linux/err.h>
#include <linux/fs.h>
#include <linux/hwmon.h>
#include <linux/hwmon-sysfs.h>
#include <linux/init.h>
#include <linux/list.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/uaccess.h>

#include <uapi/vfio_sensor_bridge.h>

struct vsb_sensor {
	u32 id;
	u32 kind;
	u32 channel;
	s64 value;
	char label[VSB_LABEL_LEN];
};

struct vsb_vm {
	struct list_head node;
	u32 vmid;
	u32 serial;   /* per-vmid monotonic counter; incremented on each SET_SCHEMA */
	u32 sensor_count;
	char name[VSB_HWMON_NAME_LEN];
	struct vsb_sensor sensors[VSB_MAX_SENSORS];

	struct device *hwmon_dev;
	struct attribute_group group;
	const struct attribute_group *groups[2];
	struct attribute **attrs;
	struct sensor_device_attribute *dev_attrs;
	char **attr_names;
	u32 attr_count;
};

static LIST_HEAD(vsb_vms);
static DEFINE_MUTEX(vsb_lock);
static struct miscdevice vsb_miscdev;

static const char *vsb_kind_prefix(u32 kind)
{
	switch (kind) {
	case VSB_SENSOR_TEMP:
		return "temp";
	case VSB_SENSOR_FAN:
		return "fan";
	case VSB_SENSOR_IN:
		return "in";
	case VSB_SENSOR_CURR:
		return "curr";
	case VSB_SENSOR_POWER:
		return "power";
	default:
		return NULL;
	}
}

static bool vsb_valid_kind(u32 kind)
{
	return vsb_kind_prefix(kind) != NULL;
}

static bool vsb_valid_channel(u32 kind, u32 channel)
{
	if (kind == VSB_SENSOR_IN)
		return channel < VSB_MAX_SENSORS;

	return channel > 0 && channel <= VSB_MAX_SENSORS;
}

static bool vsb_valid_label(const char label[VSB_LABEL_LEN])
{
	u32 i;

	if (!label[0])
		return false;

	for (i = 0; i < VSB_LABEL_LEN; i++) {
		unsigned char c = label[i];

		if (!c)
			return true;
		if (c < 0x20 || c == 0x7f)
			return false;
	}

	return false;
}

static bool vsb_valid_hwmon_name_char(char c)
{
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
	       (c >= '0' && c <= '9') || c == '_';
}

static bool vsb_valid_hwmon_name(const char name[VSB_HWMON_NAME_LEN])
{
	u32 i;

	if (!name[0])
		return true;

	for (i = 0; i < VSB_HWMON_NAME_LEN; i++) {
		if (!name[i])
			return true;
		if (!vsb_valid_hwmon_name_char(name[i]))
			return false;
	}

	return false;
}

static struct vsb_vm *vsb_find_vm(u32 vmid)
{
	struct vsb_vm *vm;

	list_for_each_entry(vm, &vsb_vms, node) {
		if (vm->vmid == vmid)
			return vm;
	}

	return NULL;
}

static ssize_t vsb_show_input(struct device *dev,
			      struct device_attribute *attr, char *buf)
{
	struct vsb_vm *vm = dev_get_drvdata(dev);
	int idx = to_sensor_dev_attr(attr)->index;
	s64 value;

	if (!vm || idx < 0 || idx >= vm->sensor_count)
		return -EINVAL;

	mutex_lock(&vsb_lock);
	value = vm->sensors[idx].value;
	mutex_unlock(&vsb_lock);

	return scnprintf(buf, PAGE_SIZE, "%lld\n", value);
}

static ssize_t vsb_show_label(struct device *dev,
			      struct device_attribute *attr, char *buf)
{
	struct vsb_vm *vm = dev_get_drvdata(dev);
	int idx = to_sensor_dev_attr(attr)->index;
	char label[VSB_LABEL_LEN];

	if (!vm || idx < 0 || idx >= vm->sensor_count)
		return -EINVAL;

	mutex_lock(&vsb_lock);
	strscpy(label, vm->sensors[idx].label, sizeof(label));
	mutex_unlock(&vsb_lock);

	return scnprintf(buf, PAGE_SIZE, "%s\n", label);
}

static void vsb_init_sensor_attr(struct sensor_device_attribute *sattr,
				 char *name, umode_t mode,
				 ssize_t (*show)(struct device *,
						 struct device_attribute *,
						 char *),
				 int index)
{
	sysfs_attr_init(&sattr->dev_attr.attr);
	sattr->dev_attr.attr.name = name;
	sattr->dev_attr.attr.mode = mode;
	sattr->dev_attr.show = show;
	sattr->dev_attr.store = NULL;
	sattr->index = index;
}

static void vsb_destroy_hwmon(struct vsb_vm *vm)
{
	u32 i;

	if (vm->hwmon_dev) {
		hwmon_device_unregister(vm->hwmon_dev);
		vm->hwmon_dev = NULL;
	}

	if (vm->attr_names) {
		for (i = 0; i < vm->attr_count; i++)
			kfree(vm->attr_names[i]);
	}

	kfree(vm->attr_names);
	kfree(vm->dev_attrs);
	kfree(vm->attrs);

	vm->attr_names = NULL;
	vm->dev_attrs = NULL;
	vm->attrs = NULL;
	vm->attr_count = 0;
}

static int vsb_register_hwmon(struct vsb_vm *vm)
{
	u32 i;
	u32 attr_idx = 0;

	vm->attr_count = vm->sensor_count * 2;
	vm->attrs = kcalloc(vm->attr_count + 1, sizeof(*vm->attrs), GFP_KERNEL);
	vm->dev_attrs = kcalloc(vm->attr_count, sizeof(*vm->dev_attrs), GFP_KERNEL);
	vm->attr_names = kcalloc(vm->attr_count, sizeof(*vm->attr_names), GFP_KERNEL);

	if (!vm->attrs || !vm->dev_attrs || !vm->attr_names)
		return -ENOMEM;

	for (i = 0; i < vm->sensor_count; i++) {
		const char *prefix = vsb_kind_prefix(vm->sensors[i].kind);
		char *input_name;
		char *label_name;

		input_name = kasprintf(GFP_KERNEL, "%s%u_input", prefix,
				       vm->sensors[i].channel);
		label_name = kasprintf(GFP_KERNEL, "%s%u_label", prefix,
				       vm->sensors[i].channel);
		if (!input_name || !label_name) {
			kfree(input_name);
			kfree(label_name);
			return -ENOMEM;
		}

		vm->attr_names[attr_idx] = input_name;
		vsb_init_sensor_attr(&vm->dev_attrs[attr_idx], input_name, 0444,
				     vsb_show_input, i);
		vm->attrs[attr_idx] = &vm->dev_attrs[attr_idx].dev_attr.attr;
		attr_idx++;

		vm->attr_names[attr_idx] = label_name;
		vsb_init_sensor_attr(&vm->dev_attrs[attr_idx], label_name, 0444,
				     vsb_show_label, i);
		vm->attrs[attr_idx] = &vm->dev_attrs[attr_idx].dev_attr.attr;
		attr_idx++;
	}

	vm->attrs[attr_idx] = NULL;
	vm->group.attrs = vm->attrs;
	vm->groups[0] = &vm->group;
	vm->groups[1] = NULL;

	vm->hwmon_dev = hwmon_device_register_with_groups(vsb_miscdev.this_device,
							  vm->name, vm,
							  vm->groups);
	if (IS_ERR(vm->hwmon_dev)) {
		int ret = PTR_ERR(vm->hwmon_dev);

		vm->hwmon_dev = NULL;
		return ret;
	}

	return 0;
}

static void vsb_free_vm(struct vsb_vm *vm)
{
	if (!vm)
		return;

	vsb_destroy_hwmon(vm);
	kfree(vm);
}

static int vsb_schema_validate(const struct vsb_schema *schema)
{
	u32 i;
	u32 j;

	if (!schema->vmid || schema->sensor_count > VSB_MAX_SENSORS ||
	    schema->reserved || !vsb_valid_hwmon_name(schema->hwmon_name))
		return -EINVAL;

	for (i = 0; i < schema->sensor_count; i++) {
		const struct vsb_sensor_desc *sensor = &schema->sensors[i];

		if (!sensor->id || sensor->reserved ||
		    !vsb_valid_kind(sensor->kind) ||
		    !vsb_valid_channel(sensor->kind, sensor->channel) ||
		    !vsb_valid_label(sensor->label))
			return -EINVAL;

		for (j = i + 1; j < schema->sensor_count; j++) {
			if (sensor->id == schema->sensors[j].id)
				return -EINVAL;
			if (sensor->kind == schema->sensors[j].kind &&
			    sensor->channel == schema->sensors[j].channel)
				return -EINVAL;
		}
	}

	return 0;
}

static s64 vsb_vm_value_or_default(const struct vsb_vm *vm, u32 id)
{
	u32 i;

	if (!vm)
		return 0;

	for (i = 0; i < vm->sensor_count; i++) {
		if (vm->sensors[i].id == id)
			return vm->sensors[i].value;
	}

	return 0;
}

static int vsb_ioctl_set_schema(unsigned long arg)
{
	struct vsb_schema *schema;
	struct vsb_vm *new_vm;
	struct vsb_vm *old_vm;
	u32 expected_serial;
	u32 i;
	int ret;

	schema = memdup_user((void __user *)arg, sizeof(*schema));
	if (IS_ERR(schema))
		return PTR_ERR(schema);

	ret = vsb_schema_validate(schema);
	if (ret)
		goto out_schema;

	new_vm = kzalloc(sizeof(*new_vm), GFP_KERNEL);
	if (!new_vm) {
		ret = -ENOMEM;
		goto out_schema;
	}

	INIT_LIST_HEAD(&new_vm->node);
	new_vm->vmid = schema->vmid;
	new_vm->sensor_count = schema->sensor_count;
	if (schema->hwmon_name[0])
		strscpy(new_vm->name, schema->hwmon_name, sizeof(new_vm->name));
	else
		snprintf(new_vm->name, sizeof(new_vm->name), "vsb_vm_%u", new_vm->vmid);

	for (i = 0; i < schema->sensor_count; i++) {
		char label[VSB_LABEL_LEN];

		memcpy(label, schema->sensors[i].label, sizeof(label));
		label[VSB_LABEL_LEN - 1] = '\0';

		new_vm->sensors[i].id = schema->sensors[i].id;
		new_vm->sensors[i].kind = schema->sensors[i].kind;
		new_vm->sensors[i].channel = schema->sensors[i].channel;
		new_vm->sensors[i].value = 0;
		strscpy(new_vm->sensors[i].label, label,
			sizeof(new_vm->sensors[i].label));
	}

	/* Step 1: snapshot current values and serial under lock. */
	mutex_lock(&vsb_lock);
	old_vm = vsb_find_vm(new_vm->vmid);
	expected_serial = old_vm ? old_vm->serial : 0;
	new_vm->serial = expected_serial + 1;
	for (i = 0; i < new_vm->sensor_count; i++)
		new_vm->sensors[i].value =
			vsb_vm_value_or_default(old_vm, new_vm->sensors[i].id);
	mutex_unlock(&vsb_lock);

	/* Step 2: register hwmon device (may sleep, lock NOT held). */
	ret = vsb_register_hwmon(new_vm);
	if (ret) {
		vsb_free_vm(new_vm);
		goto out_schema;
	}

	/*
	 * Step 3: atomically commit.  Compare the serial number seen in step 1
	 * with the current entry's serial to detect concurrent updates (ABA-
	 * safe: a remove + re-add would produce serial 1, not the same value).
	 */
	mutex_lock(&vsb_lock);
	old_vm = vsb_find_vm(new_vm->vmid);
	if ((old_vm ? old_vm->serial : 0) != expected_serial) {
		/*
		 * A concurrent set_schema or remove_vm already committed between
		 * steps 1 and 3.  Roll back to avoid replacing a newer schema.
		 */
		mutex_unlock(&vsb_lock);
		vsb_free_vm(new_vm);
		ret = -EBUSY;
		goto out_schema;
	}
	if (old_vm)
		list_replace(&old_vm->node, &new_vm->node);
	else
		list_add_tail(&new_vm->node, &vsb_vms);
	mutex_unlock(&vsb_lock);

	vsb_free_vm(old_vm);

out_schema:
	kfree(schema);
	return ret;
}

static bool vsb_vm_has_sensor_id(const struct vsb_vm *vm, u32 id)
{
	u32 i;

	for (i = 0; i < vm->sensor_count; i++) {
		if (vm->sensors[i].id == id)
			return true;
	}

	return false;
}

/* Returns 0 (invalid kind) if the sensor id is not found. */
static u32 vsb_sensor_kind_for_id(const struct vsb_vm *vm, u32 id)
{
	u32 i;

	for (i = 0; i < vm->sensor_count; i++) {
		if (vm->sensors[i].id == id)
			return vm->sensors[i].kind;
	}

	return 0;
}

static void vsb_vm_set_sensor_value(struct vsb_vm *vm, u32 id, s64 value)
{
	u32 i;

	for (i = 0; i < vm->sensor_count; i++) {
		if (vm->sensors[i].id == id) {
			vm->sensors[i].value = value;
			return;
		}
	}
}

static int vsb_ioctl_set_values(unsigned long arg)
{
	struct vsb_values *values;
	struct vsb_vm *vm;
	u32 i;
	u32 j;
	int ret = 0;

	values = memdup_user((void __user *)arg, sizeof(*values));
	if (IS_ERR(values))
		return PTR_ERR(values);

	if (!values->vmid || values->value_count > VSB_MAX_SENSORS) {
		ret = -EINVAL;
		goto out_values;
	}

	mutex_lock(&vsb_lock);
	vm = vsb_find_vm(values->vmid);
	if (!vm) {
		ret = -ENOENT;
		goto out_unlock;
	}

	for (i = 0; i < values->value_count; i++) {
		s64 v = values->values[i].value;

		if (!values->values[i].id ||
		    values->values[i].reserved ||
		    !vsb_vm_has_sensor_id(vm, values->values[i].id)) {
			ret = -EINVAL;
			goto out_unlock;
		}

		/*
		 * Reject physically impossible sensor values.  Limits match the
		 * maximum range that lm_sensors / fancontrol consumers expect:
		 *   temperature : -273 000 .. +300 000 m°C  (absolute zero to 300 °C)
		 *   fan         :       0 ..  500 000 RPM
		 *   voltage (in):  -50 000 ..  +50 000 mV
		 *   current     : -100 000 .. +100 000 mA
		 *   power       :       0 ..  1 000 000 000 µW  (1 kW)
		 *
		 * An i64::MAX injected by a malicious VM guest would confuse
		 * fan-control daemons that perform arithmetic on the raw values.
		 */
		switch (vsb_sensor_kind_for_id(vm, values->values[i].id)) {
		case VSB_SENSOR_TEMP:
			if (v < -273000 || v > 300000) {
				ret = -ERANGE;
				goto out_unlock;
			}
			break;
		case VSB_SENSOR_FAN:
			if (v < 0 || v > 500000) {
				ret = -ERANGE;
				goto out_unlock;
			}
			break;
		case VSB_SENSOR_IN:
			if (v < -50000 || v > 50000) {
				ret = -ERANGE;
				goto out_unlock;
			}
			break;
		case VSB_SENSOR_CURR:
			if (v < -100000 || v > 100000) {
				ret = -ERANGE;
				goto out_unlock;
			}
			break;
		case VSB_SENSOR_POWER:
			if (v < 0 || v > 1000000000) {
				ret = -ERANGE;
				goto out_unlock;
			}
			break;
		default:
			break;
		}

		for (j = i + 1; j < values->value_count; j++) {
			if (values->values[i].id == values->values[j].id) {
				ret = -EINVAL;
				goto out_unlock;
			}
		}
	}

	for (i = 0; i < values->value_count; i++)
		vsb_vm_set_sensor_value(vm, values->values[i].id,
					values->values[i].value);

out_unlock:
	mutex_unlock(&vsb_lock);
out_values:
	kfree(values);
	return ret;
}

static int vsb_ioctl_remove_vm(unsigned long arg)
{
	struct vsb_vm_ref ref;
	struct vsb_vm *vm;

	if (copy_from_user(&ref, (void __user *)arg, sizeof(ref)))
		return -EFAULT;

	if (!ref.vmid)
		return -EINVAL;

	mutex_lock(&vsb_lock);
	vm = vsb_find_vm(ref.vmid);
	if (vm)
		list_del(&vm->node);
	mutex_unlock(&vsb_lock);

	vsb_free_vm(vm);
	return 0;
}

static long vsb_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	/* Only privileged userspace (hostd running as root with CAP_SYS_ADMIN)
	 * may manipulate kernel hwmon state. This is defence-in-depth on top of
	 * the 0600 device mode: a misconfigured setuid binary or a leaked fd
	 * from a less-privileged child process cannot affect sensor data.
	 */
	if (!capable(CAP_SYS_ADMIN))
		return -EPERM;

	switch (cmd) {
	case VSB_IOCTL_SET_SCHEMA:
		return vsb_ioctl_set_schema(arg);
	case VSB_IOCTL_SET_VALUES:
		return vsb_ioctl_set_values(arg);
	case VSB_IOCTL_REMOVE_VM:
		return vsb_ioctl_remove_vm(arg);
	default:
		return -ENOTTY;
	}
}

static const struct file_operations vsb_fops = {
	.owner = THIS_MODULE,
	.unlocked_ioctl = vsb_ioctl,
	.llseek = noop_llseek,
};

static struct miscdevice vsb_miscdev = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = "vfio-sensor-bridge",
	.fops = &vsb_fops,
	.mode = 0600,
};

static int __init vsb_init(void)
{
	return misc_register(&vsb_miscdev);
}

static void __exit vsb_exit(void)
{
	struct vsb_vm *vm;
	struct vsb_vm *tmp;

	mutex_lock(&vsb_lock);
	list_for_each_entry_safe(vm, tmp, &vsb_vms, node) {
		list_del(&vm->node);
		mutex_unlock(&vsb_lock);
		vsb_free_vm(vm);
		mutex_lock(&vsb_lock);
	}
	mutex_unlock(&vsb_lock);

	misc_deregister(&vsb_miscdev);
}

module_init(vsb_init);
module_exit(vsb_exit);

MODULE_AUTHOR("vfio-sensor-bridge contributors");
MODULE_DESCRIPTION("VFIO sensor bridge hwmon driver");
MODULE_LICENSE("GPL");
