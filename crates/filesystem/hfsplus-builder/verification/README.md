# HFS+ builder verification

The Kani placement harness covers the interval admission portion of H-V07. It checks up to eight placements with arbitrary `u64` offsets and lengths against an independent widened-arithmetic prefix model. The harness calls the production admission function. Hosted tests exercise exact reads across boundaries, large zero ranges, nonseekable sinks, source faults, and cancellation. Native image construction and the remaining H-V07 evidence are tracked in the repository obligation registry.

The Kani allocation harnesses cover part of H-V06 for volumes with up to 32 blocks, 512-byte and 4096-byte allocation sizes, and any partial device tail below one block. An exact initial reservation can reach any admitted cursor. From that state the transition harnesses compare a full-width `u64` request and resulting free count against an independent `u128` range model. The bitmap harnesses compare every encoded bit at any admitted cursor to the same model. Larger bitmaps and complete native volumes have separate test evidence.

Install Kani 0.68.0 and run `cargo kani setup` before running `bash check.sh` from this directory.
