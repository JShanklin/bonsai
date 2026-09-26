// Runs before rendering. Maps the `chip` choice to its Xtensa target triple.
// Note: Xtensa targets need the esp Rust toolchain (`espup install`).

let chip = variable::get("chip");

let target = if chip == "s3" {
    "xtensa-esp32s3-none-elf"
} else {
    "xtensa-esp32-none-elf"
};

variable::set("target", target);
