//! 页面级确认弹窗：「删除计划」。
//!
//! 由左侧顶栏「更多操作 → 删除」触发。确认后通过 `on_confirm` 回调
//! 通知页面层执行删除（删消息 + 删计划 + 返回列表页），弹窗本身不持有仓库。

use dioxus::prelude::*;

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};

#[component]
pub fn DeletePlanDialog(
    open: Signal<bool, SyncStorage>,
    on_confirm: EventHandler<()>,
) -> Element {
    rsx! {
        AlertDialog {
            open: open(),
            on_open_change: move |v: bool| open.set(v),
            AlertDialogTitle { "删除计划？" }
            AlertDialogDescription {
                "确定要删除这个计划吗？所有相关的对话消息也会被删除，操作无法撤销。"
            }
            AlertDialogActions {
                AlertDialogCancel { "取消" }
                AlertDialogAction {
                    on_click: move |_| {
                        open.set(false);
                        on_confirm.call(());
                    },
                    "删除"
                }
            }
        }
    }
}
