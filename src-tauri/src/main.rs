// 发布构建不带控制台窗口；debug 下保留控制台以便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    livestream_pet_zhuzhu::run();
}
