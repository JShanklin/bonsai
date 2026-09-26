// Runs before any file is rendered. Turns the `chip` choice into the concrete
// build settings the templated files need, so they don't each have to branch.

let chip = variable::get("chip");

// RP2350 is Cortex-M33 (thumbv8m, hard-float); RP2040 is Cortex-M0+ (thumbv6m).
let target = if chip == "rp2350" {
    "thumbv8m.main-none-eabihf"
} else {
    "thumbv6m-none-eabi"
};

// The name probe-rs / picotool expect for `--chip`.
let probe_chip = if chip == "rp2350" { "RP2350" } else { "RP2040" };

variable::set("target", target);
variable::set("probe_chip", probe_chip);
