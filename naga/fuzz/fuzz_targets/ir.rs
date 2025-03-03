#![no_main]
#[cfg(not(any(target_arch = "wasm32", target_os = "ios", target_os = "tvos")))]
mod fuzz {
    use libfuzzer_sys::fuzz_target;

    fuzz_target!(|module: naga::Module| {
        use naga::valid as v;
        // Check if the module validates without errors.
        //TODO: may also fuzz the flags and capabilities
        let mut validator =
            v::Validator::new(v::ValidationFlags::all(), v::Capabilities::default());
        let _result = validator.validate(&module);
    });
}
