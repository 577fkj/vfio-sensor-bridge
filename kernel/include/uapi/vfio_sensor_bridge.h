#ifndef _UAPI_VFIO_SENSOR_BRIDGE_H
#define _UAPI_VFIO_SENSOR_BRIDGE_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define VSB_MAX_SENSORS 128
#define VSB_LABEL_MAX 128
#define VSB_LABEL_LEN (VSB_LABEL_MAX + 1)
#define VSB_HWMON_NAME_LEN 32

enum vsb_sensor_kind {
	VSB_SENSOR_TEMP = 1,
	VSB_SENSOR_FAN = 2,
	VSB_SENSOR_IN = 3,
	VSB_SENSOR_CURR = 4,
	VSB_SENSOR_POWER = 5,
};

struct vsb_sensor_desc {
	__u32 id;
	__u32 kind;
	__u32 channel;
	__u32 reserved;
	char label[VSB_LABEL_LEN];
};

struct vsb_schema {
	__u32 vmid;
	__u32 sensor_count;
	char hwmon_name[VSB_HWMON_NAME_LEN];
	__u32 reserved;
	struct vsb_sensor_desc sensors[VSB_MAX_SENSORS];
};

struct vsb_sensor_value {
	__u32 id;
	__u32 reserved;
	__s64 value;
};

struct vsb_values {
	__u32 vmid;
	__u32 value_count;
	struct vsb_sensor_value values[VSB_MAX_SENSORS];
};

struct vsb_vm_ref {
	__u32 vmid;
};

#define VSB_IOCTL_MAGIC 'V'
#define VSB_IOCTL_SET_SCHEMA _IO(VSB_IOCTL_MAGIC, 0x01)
#define VSB_IOCTL_SET_VALUES _IOW(VSB_IOCTL_MAGIC, 0x02, struct vsb_values)
#define VSB_IOCTL_REMOVE_VM _IOW(VSB_IOCTL_MAGIC, 0x03, struct vsb_vm_ref)

#endif
