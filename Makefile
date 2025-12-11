run:
	cargo run --release

demo:
	cargo build
	./target/debug/repl-net-rs --runtime server --addr 127.0.0.1:4000 & \
	sleep 0.2; \
	./target/debug/repl-net-rs --runtime client --addr 127.0.0.1:4000 & \
	./target/debug/repl-net-rs --runtime client --addr 127.0.0.1:4000 & \
	wait

demo-malicious:
	cargo build
	./target/debug/repl-net-rs --runtime server --addr 127.0.0.1:4000 & \
	sleep 0.2; \
	./target/debug/repl-net-rs --runtime client --addr 127.0.0.1:4000 & \
	./target/debug/repl-net-rs --runtime malicious --addr 127.0.0.1:4000 & \
	wait