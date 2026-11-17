#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A ggml-base.dll da release b12387 instala um terminate handler no load.
    // Com `GGML_NO_BACKTRACE` ela pula essa instalação; sem isso, descarregar e
    // recarregar a DLL no mesmo processo aborta (llama.cpp#15752). O pin em
    // `ai::ggml::ffi` é a correção de verdade; isto é o cinto.
    // SAFETY: chamado antes de qualquer outra thread ou LoadLibrary da ggml.
    unsafe { std::env::set_var("GGML_NO_BACKTRACE", "1") };
    aether_lib::run()
}