//! 各技能（`PlanCategory`）的**作业规范段**。
//!
//! 执行期由 `step_system_prompt` 追加到 system prompt 尾部；**这里是「加技能时唯一要动的地步」**
//! —— 枚举在 `plan/category.rs`，文案在这里（`category_rule_section` 把两者对上）。
//!
//! 内容纪律（见设计稿 `docs/planned-agent/flexible-plan-category.md` §5.2）：只写该技能的
//! **作业纪律与最常见的坑**，**不写具体工具用法**（那属工具 `description`）、**不写个案**（防过拟合）。

use super::super::super::plan::category::PlanCategory;

/// 浏览器自动化。
const BROWSER_RULES: &str = "\
## 计划类型：浏览器自动化（作业规范）
- 页面内容与元素状态以工具实际返回为准，不得凭 URL、惯例或经验臆测页面结构。
- 采集到的内容先落盘再分析，不要在一次回答里既抓又分析又输出。
- 涉及登录态 / 分页 / 弹窗时，先确认当前处于目标页面状态再执行下一步。
- 需要人工介入（验证码、扫码、短信）时如实报告并停下，不伪造成功、不跳过。
";

/// 文件与数据处理。
const FILE_RULES: &str = "\
## 计划类型：文件与数据处理（作业规范）
- 所有路径 / 数据来源来自本次参数或前序结果，不臆造。
- 读 / 改之前先看现状：是否存在、当前内容与结构（表头、编码、行尾、大小）。
- 大文件 / 大表按行或分块读，不整读进上下文；中间结果落盘。
- 保留原文实体（人名、编号、术语、格式标记）；非本步目标不改写、不翻译、不填充缺失值。
- 统计 / 转换的口径要显式（去重、空值、单位、时间格式），并写进产出。
- 写 / 改 / 批量操作（移动、删除、改名）先确认范围与目标，操作后核对结果。
";

/// 信息检索与研究。
const RESEARCH_RULES: &str = "\
## 计划类型：信息检索与研究（作业规范）
- 多来源分别落盘再汇总，不要在中间状态里混合。
- 标注来源；冲突信息如实并列，不擅自取舍或调和。
- 检索不到时如实报告「未找到」，不臆造结论。
";

/// 开发与运维。
const DEV_RULES: &str = "\
## 计划类型：开发与运维（作业规范）
- 命令跨平台，按「运行环境」段判断 shell / 可用命令，不硬编码。
- 破坏性操作（覆盖、删除、杀进程、改系统配置、推送）先确认目标再执行。
- 改代码 / 配置前先读现状，改后核对；不整文件重写。
- 执行结果以命令实际返回为准，不臆测。
";

/// 移动端自动化。
const DEVICE_RULES: &str = "\
## 计划类型：移动端自动化（作业规范）
- 设备状态以工具返回为准；操作前确认已连接、已解锁、处于目标界面。
- 失败先查连接 / 权限 / 驱动，不假设操作成功。
- 需要人工介入时如实报告并停下。
";

/// 取该技能的作业规范段；`Other` / 未知**无规范段**（返回 `None`，不追加）。
pub(crate) fn category_rule_section(category: PlanCategory) -> Option<&'static str> {
    match category {
        PlanCategory::Browser => Some(BROWSER_RULES),
        PlanCategory::File => Some(FILE_RULES),
        PlanCategory::Research => Some(RESEARCH_RULES),
        PlanCategory::Dev => Some(DEV_RULES),
        PlanCategory::Device => Some(DEVICE_RULES),
        PlanCategory::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个非 `Other` 技能都有规范段，且都带标题行。
    #[test]
    fn every_non_other_category_has_a_rule_section() {
        for category in PlanCategory::ALL {
            let section = category_rule_section(category);
            match category {
                PlanCategory::Other => assert!(section.is_none(), "Other 不该有规范段"),
                _ => {
                    let section = section.expect("非 Other 应有规范段");
                    assert!(section.starts_with("## 计划类型："), "{section}");
                }
            }
        }
    }
}
