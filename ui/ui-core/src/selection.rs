//! 多选列表：选中集合 + 全选/反选/清空。
//!
//! # 参考 PCL
//!
//! PCL 的 `MyListItem` 有 `Checked` 属性（多选态），配合顶部的"全选"复选框。
//! 本实现把"选中集合"从控件里抽出来单独管理 —— 因为选中状态需要**跨帧持久**
//! 且可能被多处访问（列表、工具栏、状态栏），放在控件内部不方便。
//!
//! # 与单选的区别
//!
//! | | 单选 | 多选 |
//! |---|---|---|
//! | 状态 | `Option<T>` | `HashSet<T>` |
//! | 点击已选项 | 保持选中 | **取消选中**（切换） |
//! | Ctrl+点击 | — | 在不影响其他项的情况下切换 |
//! | 工具栏 | 无 | "已选 N 项" + 全选/清空 |

use std::collections::HashSet;

/// 多选状态容器。由调用方跨帧持有。
#[derive(Debug, Default, Clone)]
pub struct Selection {
    items: HashSet<String>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否选中。
    pub fn is_selected(&self, id: &str) -> bool {
        self.items.contains(id)
    }

    /// 选中数量。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 切换某项的选中状态。返回**新的**选中状态。
    pub fn toggle(&mut self, id: &str) -> bool {
        if self.items.remove(id) {
            false
        } else {
            self.items.insert(id.to_owned());
            true
        }
    }

    /// 直接设为选中。
    pub fn select(&mut self, id: &str) {
        self.items.insert(id.to_owned());
    }

    /// 取消选中。
    pub fn deselect(&mut self, id: &str) {
        self.items.remove(id);
    }

    /// 清空选择。
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// 全选（给定全部候选 id）。
    pub fn select_all(&mut self, ids: &[String]) {
        self.items.clear();
        for id in ids {
            self.items.insert(id.clone());
        }
    }

    /// 反选（给定全部候选 id）。
    pub fn invert(&mut self, ids: &[String]) {
        let current = self.items.clone();
        self.items.clear();
        for id in ids {
            if !current.contains(id) {
                self.items.insert(id.clone());
            }
        }
    }

    /// 已选中的 id（**排序后**，保证 UI 显示稳定）。
    pub fn selected(&self) -> Vec<String> {
        let mut v: Vec<String> = self.items.iter().cloned().collect();
        v.sort();
        v
    }

    /// 人类可读的摘要，例如 "已选 3 项"。
    pub fn summary(&self) -> String {
        match self.items.len() {
            0 => "未选择".to_owned(),
            1 => "已选 1 项".to_owned(),
            n => format!("已选 {n} 项"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn toggle_adds_and_removes() {
        let mut s = Selection::new();
        assert!(s.toggle("a"));
        assert!(s.is_selected("a"));
        assert!(!s.toggle("a"));
        assert!(!s.is_selected("a"));
    }

    #[test]
    fn toggle_is_independent_per_item() {
        let mut s = Selection::new();
        s.toggle("a");
        s.toggle("b");
        assert_eq!(s.len(), 2);
        s.toggle("a");
        assert_eq!(s.len(), 1);
        assert!(s.is_selected("b"));
    }

    #[test]
    fn select_all_and_clear() {
        let mut s = Selection::new();
        s.select_all(&ids(&["a", "b", "c"]));
        assert_eq!(s.len(), 3);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn invert_flips_selection() {
        let mut s = Selection::new();
        s.select("b");
        s.invert(&ids(&["a", "b", "c"]));
        assert_eq!(s.selected(), ids(&["a", "c"]));
    }

    #[test]
    fn invert_with_empty_selection_selects_all() {
        let mut s = Selection::new();
        s.invert(&ids(&["a", "b"]));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn selected_is_sorted() {
        let mut s = Selection::new();
        s.select("z");
        s.select("a");
        s.select("m");
        assert_eq!(
            s.selected(),
            ids(&["a", "m", "z"]),
            "输出必须有序，保证 UI 稳定"
        );
    }

    #[test]
    fn summary_text() {
        let mut s = Selection::new();
        assert_eq!(s.summary(), "未选择");
        s.select("a");
        assert_eq!(s.summary(), "已选 1 项");
        s.select("b");
        assert_eq!(s.summary(), "已选 2 项");
    }

    #[test]
    fn select_all_replaces_previous() {
        let mut s = Selection::new();
        s.select("old");
        s.select_all(&ids(&["a"]));
        assert!(!s.is_selected("old"), "全选应替换原有选择");
        assert!(s.is_selected("a"));
    }
}
