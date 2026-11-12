#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A PDF's text, read in a process of its own (attachments.rs).
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, pdf, out] = args.as_slice() {
        if flag == "--pdf-text" {
            return relay_lib::pdf_text_main(pdf, out);
        }
    }
    relay_lib::run()
}
