// SPDX-License-Identifier: GPL-2.0-or-later
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "vfio_sensor_bridge.h"

_Static_assert(VSB_LABEL_MAX == 128, "unexpected sensor label limit");

static void set_label(char dst[VSB_LABEL_LEN], const char *src)
{
	snprintf(dst, VSB_LABEL_LEN, "%s", src);
}

static void set_sensor(struct vsb_sensor_desc *sensor, __u32 id, __u32 kind,
		       __u32 channel, const char *label)
{
	memset(sensor, 0, sizeof(*sensor));
	sensor->id = id;
	sensor->kind = kind;
	sensor->channel = channel;
	set_label(sensor->label, label);
}

static int find_hwmon(const char *name, char *out, size_t out_len)
{
	DIR *dir = opendir("/sys/class/hwmon");
	struct dirent *entry;

	if (!dir)
		return -1;

	while ((entry = readdir(dir)) != NULL) {
		char name_path[512];
		char value[128] = { 0 };
		FILE *file;

		if (entry->d_name[0] == '.')
			continue;

		snprintf(name_path, sizeof(name_path), "/sys/class/hwmon/%s/name",
			 entry->d_name);
		file = fopen(name_path, "r");
		if (!file)
			continue;

		if (fgets(value, sizeof(value), file)) {
			value[strcspn(value, "\n")] = '\0';
		}
		if (strcmp(value, name) == 0) {
			fclose(file);
			snprintf(out, out_len, "/sys/class/hwmon/%s",
				 entry->d_name);
			closedir(dir);
			return 0;
		}
		fclose(file);
	}

	closedir(dir);
	return -1;
}

static void print_attr(const char *base, const char *attr)
{
	char path[512];
	char value[128];
	FILE *file;

	snprintf(path, sizeof(path), "%s/%s", base, attr);
	file = fopen(path, "r");
	if (!file) {
		printf("%s=<missing>\n", attr);
		return;
	}

	if (fgets(value, sizeof(value), file)) {
		value[strcspn(value, "\n")] = '\0';
		printf("%s=%s\n", attr, value);
	}
	fclose(file);
}

int main(int argc, char **argv)
{
	const char *dev_path = "/dev/vfio-sensor-bridge";
	__u32 vmid = 9000;
	int keep = 0;
	struct vsb_schema schema;
	struct vsb_values values;
	struct vsb_vm_ref ref;
	char hwmon_name[VSB_HWMON_NAME_LEN];
	char hwmon_path[512];
	int fd;
	int i;

	for (i = 1; i < argc; i++) {
		if (strcmp(argv[i], "--keep") == 0) {
			keep = 1;
		} else if (strcmp(argv[i], "--vmid") == 0 && i + 1 < argc) {
			char *end = NULL;
			unsigned long parsed;

			errno = 0;
			parsed = strtoul(argv[++i], &end, 10);
			if (errno || *end != '\0' || parsed == 0 ||
			    parsed > UINT32_MAX) {
				fprintf(stderr, "invalid VMID\n");
				return 2;
			}
			vmid = (__u32)parsed;
		} else if (strcmp(argv[i], "--device") == 0 && i + 1 < argc) {
			dev_path = argv[++i];
		} else {
			fprintf(stderr,
				"usage: %s [--vmid VMID] [--device PATH] [--keep]\n",
				argv[0]);
			return 2;
		}
	}

	fd = open(dev_path, O_RDWR);
	if (fd < 0) {
		perror(dev_path);
		return 1;
	}

	memset(&schema, 0, sizeof(schema));
	schema.vmid = vmid;
	schema.sensor_count = 5;
	set_sensor(&schema.sensors[0], 1, VSB_SENSOR_TEMP, 1, "smoke_temp");
	set_sensor(&schema.sensors[1], 2, VSB_SENSOR_FAN, 1, "smoke_fan");
	set_sensor(&schema.sensors[2], 3, VSB_SENSOR_IN, 0, "smoke_voltage");
	set_sensor(&schema.sensors[3], 4, VSB_SENSOR_CURR, 1, "smoke_current");
	set_sensor(&schema.sensors[4], 5, VSB_SENSOR_POWER, 1, "smoke_power");

	if (ioctl(fd, VSB_IOCTL_SET_SCHEMA, &schema) < 0) {
		perror("VSB_IOCTL_SET_SCHEMA");
		close(fd);
		return 1;
	}

	memset(&values, 0, sizeof(values));
	values.vmid = vmid;
	values.value_count = 5;
	values.values[0].id = 1;
	values.values[0].value = 42000;
	values.values[1].id = 2;
	values.values[1].value = 1500;
	values.values[2].id = 3;
	values.values[2].value = 12000;
	values.values[3].id = 4;
	values.values[3].value = 2500;
	values.values[4].id = 5;
	values.values[4].value = 75000000;

	if (ioctl(fd, VSB_IOCTL_SET_VALUES, &values) < 0) {
		perror("VSB_IOCTL_SET_VALUES");
		close(fd);
		return 1;
	}

	snprintf(hwmon_name, sizeof(hwmon_name), "vsb_vm_%u", vmid);
	if (find_hwmon(hwmon_name, hwmon_path, sizeof(hwmon_path)) < 0) {
		fprintf(stderr, "hwmon device %s not found\n", hwmon_name);
		close(fd);
		return 1;
	}

	printf("created %s\n", hwmon_path);
	print_attr(hwmon_path, "temp1_input");
	print_attr(hwmon_path, "temp1_label");
	print_attr(hwmon_path, "fan1_input");
	print_attr(hwmon_path, "fan1_label");
	print_attr(hwmon_path, "in0_input");
	print_attr(hwmon_path, "in0_label");
	print_attr(hwmon_path, "curr1_input");
	print_attr(hwmon_path, "curr1_label");
	print_attr(hwmon_path, "power1_input");
	print_attr(hwmon_path, "power1_label");

	if (!keep) {
		memset(&ref, 0, sizeof(ref));
		ref.vmid = vmid;
		if (ioctl(fd, VSB_IOCTL_REMOVE_VM, &ref) < 0) {
			perror("VSB_IOCTL_REMOVE_VM");
			close(fd);
			return 1;
		}
		printf("removed %s\n", hwmon_name);
	}

	close(fd);
	return 0;
}
