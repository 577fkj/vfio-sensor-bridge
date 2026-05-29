.PHONY: build check kernel install-host install-agent smoke clean

build:
	cargo build --workspace

check:
	cargo check --workspace

kernel:
	$(MAKE) -C kernel

install-host:
	sh packaging/scripts/install-host.sh

install-agent:
	sh packaging/scripts/install-agent.sh

smoke:
	vsbctl smoke

clean:
	$(MAKE) -C kernel clean
	cargo clean
