# HFS+ builder verification

The Kani placement harness covers the interval admission portion of H-V07. It checks up to eight placements with arbitrary `u64` offsets and lengths against an independent widened-arithmetic prefix model. The harness calls the production admission function. Hosted tests exercise exact reads across boundaries, large zero ranges, nonseekable sinks, source faults, and cancellation. Native image construction and the remaining H-V07 evidence are tracked in the repository obligation registry.

Install Kani 0.68.0 and run `cargo kani setup` before running `bash check.sh` from this directory.
