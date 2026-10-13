//! `cargo bench -p opentui-bench` facade over the rig main (the rig is
//! a plain bencher: stable toolchain, deterministic iteration counts).

fn main() -> anyhow::Result<()> {
    opentui_bench::bench_main()
}
