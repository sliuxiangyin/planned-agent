//! 计划分类（技能 / 场景口径）。
//!
//! 回答一个问题：**这个计划属于哪门技能 / 面向哪种介质**。
//!
//! 判据是**操作的「介质 / 界面」**（网页 / 本地文件与数据 / 命令行与代码 / 手机 / 网络信息），
//! **不是处理环节**（读取、分析、写入、转换都是环节）—— 一条作业链（读目录 → 读文件 →
//! 分析 → 写回）里换的是环节、不变的是介质，它属于**同一个技能**。
//!
//! 由规划步（`flexible_plan`）选中，与 `steps` 一起定稿；执行期据此在 system prompt 尾部
//! 追加该技能的**作业规范段**（文案见 `exec/prompt/skills.rs`，本模块只管类型）。
//!
//! **单值**（一个计划只属于一个分类）—— 它是「归属」，不是标签。
//! 需要「涉及哪些介质」这类检索需求时另开多值 tag，不要把本枚举变多值。
//!
//! 向后兼容：模板 JSON 缺字段 / `null` ⇒ `None` ⇒ 不加规则段；未知取值 ⇒ [`PlanCategory::Other`]。
//! 设计稿见 `docs/planned-agent/flexible-plan-category.md`。

use serde::{Deserialize, Serialize};

/// 计划分类：这个计划属于哪门技能 / 面向哪种介质。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanCategory {
    /// 浏览器自动化：网页上的打开 / 点击 / 填表 / 抓取 / 登录态（含验证码）。
    Browser,
    /// 文件与数据处理：本地文件 / 数据的读写、清洗、统计、转换、落盘。
    File,
    /// 信息检索与研究：网络多源检索、汇总、对比。
    Research,
    /// 开发与运维：命令行 / 代码 / 系统。
    Dev,
    /// 移动端自动化：手机 / App。
    Device,
    /// 其他：无法归入以上（不加规范段）；**未知取值也落到这里**。
    #[serde(other)]
    Other,
}

impl PlanCategory {
    /// 全部合法取值（`Other` 在末位）。
    pub const ALL: [PlanCategory; 6] = [
        PlanCategory::Browser,
        PlanCategory::File,
        PlanCategory::Research,
        PlanCategory::Dev,
        PlanCategory::Device,
        PlanCategory::Other,
    ];

    /// 落库 / 提示词用的英文标识。
    ///
    /// 刻意与 [`planned_agent_core::tool_registry::ToolCategory`] 的标识**同名**（能对上的那几个），
    /// 方便将来把「计划分类 → 默认工具白名单」接起来。
    pub fn as_str(self) -> &'static str {
        match self {
            PlanCategory::Browser => "Browser",
            PlanCategory::File => "File",
            PlanCategory::Research => "Research",
            PlanCategory::Dev => "Dev",
            PlanCategory::Device => "Device",
            PlanCategory::Other => "Other",
        }
    }

    /// 中文名（UI 展示用）。
    pub fn label(self) -> &'static str {
        match self {
            PlanCategory::Browser => "浏览器自动化",
            PlanCategory::File => "文件与数据处理",
            PlanCategory::Research => "信息检索与研究",
            PlanCategory::Dev => "开发与运维",
            PlanCategory::Device => "移动端自动化",
            PlanCategory::Other => "其他",
        }
    }

    /// 由英文标识解析；未知返回 `None`（不报错）。
    ///
    /// 与 serde 的 `#[serde(other)]` 同义：认不出按「无法归类」处理。
    pub fn from_name(name: &str) -> Option<PlanCategory> {
        PlanCategory::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_and_from_name_round_trip() {
        for category in PlanCategory::ALL {
            assert_eq!(PlanCategory::from_name(category.as_str()), Some(category));
        }
        assert_eq!(PlanCategory::from_name("Browser "), None);
        assert_eq!(PlanCategory::from_name("browser"), None);
    }

    #[test]
    fn label_is_present_for_every_variant() {
        for category in PlanCategory::ALL {
            assert!(!category.label().is_empty());
            assert!(!category.as_str().is_empty());
        }
    }

    /// 英文标识与 `ToolCategory` 对齐（能对上的那几个），改一处要改两处。
    #[test]
    fn identifiers_match_tool_category_where_applicable() {
        assert_eq!(PlanCategory::Browser.as_str(), "Browser");
        assert_eq!(PlanCategory::File.as_str(), "File");
        assert_eq!(PlanCategory::Dev.as_str(), "Dev");
        assert_eq!(PlanCategory::Device.as_str(), "Device");
    }

    /// 反序列化：认识的名字映射到变体；**未知取值落到 `Other`**（容错，不报错）。
    #[test]
    fn deserializes_known_and_unknown_names() {
        assert_eq!(
            serde_json::from_str::<PlanCategory>("\"File\"").unwrap(),
            PlanCategory::File
        );
        assert_eq!(
            serde_json::from_str::<PlanCategory>("\"SomethingNew\"").unwrap(),
            PlanCategory::Other
        );
    }

    #[test]
    fn serializes_to_identifier() {
        assert_eq!(
            serde_json::to_string(&PlanCategory::Browser).unwrap(),
            "\"Browser\""
        );
        assert_eq!(serde_json::to_string(&PlanCategory::Other).unwrap(), "\"Other\"");
    }
}
