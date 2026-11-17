//! 데스크톱 진입점. 앱 본체는 lib.rs의 `run()`이다 (모바일 빌드가 같은 코드를 라이브러리로 쓴다).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    premium_discount_desktop_lib::run()
}
