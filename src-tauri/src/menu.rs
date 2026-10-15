//! 菜单定义（DEV.md §3.3：托盘菜单与角色右键菜单「共享同一份菜单定义」）。
//!
//! 这里是**唯一一份**定义：
//!  * 托盘那边用它在 Rust 里构建原生菜单；
//!  * 前端通过 `menu_items` 命令拿同一份来渲染自绘菜单。
//!
//! 这样加/改一项菜单只需要动这一个文件，不会出现「托盘有、右键没有」
//! 或者两边文案不一致的情况。
//!
//! 置顶项是**有状态**的：文案要随当前是否置顶在「置顶窗口」和「取消置顶」
//! 之间切换。所以 `entries()` 需要传入当前状态，而不是返回一个静态列表。

use serde::Serialize;

/// 置顶开关的菜单项 id
pub const MENU_TOGGLE_TOPMOST: &str = "toggle-topmost";
/// 退出的菜单项 id
pub const MENU_QUIT: &str = "quit";

const LABEL_TOPMOST_ENABLE: &str = "置顶窗口";
const LABEL_TOPMOST_DISABLE: &str = "取消置顶";
const LABEL_QUIT: &str = "退出";

/// 一项菜单。`id` 用于分发，`label` 是给用户看的文案。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuEntry {
    pub id: String,
    pub label: String,
}

/// 按当前状态生成菜单项。
///
/// 顺序即显示顺序：置顶在前，退出在后（退出是破坏性操作，放最后更安全）。
pub fn entries(always_on_top: bool) -> Vec<MenuEntry> {
    vec![
        MenuEntry {
            id: MENU_TOGGLE_TOPMOST.to_owned(),
            label: if always_on_top {
                LABEL_TOPMOST_DISABLE
            } else {
                LABEL_TOPMOST_ENABLE
            }
            .to_owned(),
        },
        MenuEntry {
            id: MENU_QUIT.to_owned(),
            label: LABEL_QUIT.to_owned(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_label_follows_state() {
        let off = entries(false);
        assert_eq!(off[0].label, "置顶窗口");
        let on = entries(true);
        assert_eq!(on[0].label, "取消置顶");
    }

    #[test]
    fn id_and_order_are_stable() {
        // id 是前后端分发的契约，顺序是显示顺序，两者都不该随状态变化
        for state in [false, true] {
            let items = entries(state);
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].id, MENU_TOGGLE_TOPMOST);
            assert_eq!(items[1].id, MENU_QUIT);
            assert_eq!(items[1].label, "退出");
        }
    }
}
