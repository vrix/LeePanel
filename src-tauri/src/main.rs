// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
  match std::env::args().nth(1).as_deref() {
    Some("--mcp") => app_lib::run_mcp(),
    Some("--mcp-version") => println!("{}", app_lib::mcp_version()),
    _ => app_lib::run(),
  }
}
