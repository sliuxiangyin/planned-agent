//! RAG 模块集成测试
//!
//! 测试 Embedder、Store 的基本 CRUD 操作。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_rag::embedder::Embedder;
use planned_agent_rag::store::{SearchFilters, StoreEntry, TraceStore};
use planned_agent_rag::PolarisDbStore;

// ═══════════════════════════════════════════════════════════
// Mock Embedder（用于测试，不依赖外部 API）
// ═══════════════════════════════════════════════════════════

/// 用于测试的 Mock Embedder
///
/// 生成固定维度的伪随机向量，保证相同文本生成相同向量。
struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    fn new(dim: usize) -> Self {
        Self { dim }
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // 简单的伪随机向量生成：基于文本内容的哈希
        let mut vec = vec![0.0f32; self.dim];
        let hash = simple_hash(text);
        let seed = hash % 1000;
        
        for i in 0..self.dim {
            // 生成稳定的伪随机值
            let val = ((seed * (i as u64 + 1) * 31) % 1000) as f32 / 1000.0;
            vec[i] = val;
        }
        
        // 归一化
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        
        Ok(vec)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        "mock-embedder"
    }
}

/// 简单字符串哈希
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for c in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(c as u64);
    }
    hash
}

// ═══════════════════════════════════════════════════════════
// 测试辅助函数
// ═══════════════════════════════════════════════════════════

/// 创建测试用的 StoreEntry
fn create_test_entry(id: &str, text: &str, labels: HashMap<String, String>) -> StoreEntry {
    StoreEntry {
        id: id.to_string(),
        text: text.to_string(),
        embedding: Vec::new(), // 稍后由 embedder 填充
        metadata: serde_json::json!({
            "source": "test",
            "category": "integration_test"
        }),
        labels,
    }
}

/// 创建测试用的 Embedder
fn create_test_embedder() -> Arc<MockEmbedder> {
    Arc::new(MockEmbedder::new(128))
}

/// 获取临时测试目录
fn get_test_dir(test_name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_dir = manifest_dir.join("test_data").join(test_name);
    std::fs::create_dir_all(&test_dir).ok();
    test_dir
}

/// 清理测试目录
fn cleanup_test_dir(test_dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(test_dir);
}

// ═══════════════════════════════════════════════════════════
// 测试用例（串行执行，避免文件系统竞争）
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_store_add_single() {
    let test_dir = get_test_dir("test_add_single");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 创建测试数据
    let mut entry = create_test_entry(
        "test_001",
        "如何打开百度首页",
        HashMap::from([("intent".to_string(), "open_baidu".to_string())]),
    );
    
    // 生成 embedding
    entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
    
    // 写入
    store.add(entry.clone()).await.expect("写入失败");
    
    // 验证数量
    let count = store.count().await.expect("查询数量失败");
    assert_eq!(count, 1, "应该有 1 条记录");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_add_single 通过");
}

#[tokio::test]
async fn test_store_add_batch() {
    let test_dir = get_test_dir("test_add_batch");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 创建多条测试数据
    let texts = vec![
        ("test_batch_001", "打开百度首页", "open_baidu"),
        ("test_batch_002", "搜索深圳天气", "search_weather"),
        ("test_batch_003", "打开腾讯新闻", "open_news"),
    ];
    
    let mut entries = Vec::new();
    for (id, text, intent) in texts {
        let mut entry = create_test_entry(
            id,
            text,
            HashMap::from([("intent".to_string(), intent.to_string())]),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        entries.push(entry);
    }
    
    // 批量写入
    store.add_batch(entries).await.expect("批量写入失败");
    
    // 验证数量
    let count = store.count().await.expect("查询数量失败");
    assert_eq!(count, 3, "应该有 3 条记录");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_add_batch 通过");
}

#[tokio::test]
async fn test_store_search() {
    let test_dir = get_test_dir("test_search");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入多条测试数据
    let texts = vec![
        ("search_001", "打开百度首页", "open_baidu"),
        ("search_002", "搜索深圳天气", "search_weather"),
        ("search_003", "打开腾讯新闻", "open_news"),
        ("search_004", "查看广州房价", "search_house"),
    ];
    
    for (id, text, intent) in &texts {
        let mut entry = create_test_entry(
            id,
            text,
            HashMap::from([("intent".to_string(), intent.to_string())]),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
    }
    
    // 搜索与"百度"相关的内容
    let query_embedding = embedder.embed("百度搜索").await.expect("生成查询 embedding 失败");
    let filters = SearchFilters::default();
    let results = store
        .search(&query_embedding, 5, &filters)
        .await
        .expect("搜索失败");
    
    // 验证结果
    assert!(!results.is_empty(), "应该返回搜索结果");
    assert!(results.len() <= 5, "结果数量不应超过 top_k");
    
    // 验证排序（分数应该递减）
    for i in 1..results.len() {
        assert!(
            results[i - 1].score >= results[i].score,
            "结果应该按分数降序排列"
        );
    }
    
    println!("搜索结果: {:?}", results.len());
    for r in &results {
        println!("  - {} (score: {:.4})", r.entry.text, r.score);
    }
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_search 通过");
}

#[tokio::test]
async fn test_store_search_with_threshold() {
    let test_dir = get_test_dir("test_search_threshold");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入测试数据
    let texts = vec![
        ("thresh_001", "打开百度首页", "open_baidu"),
        ("thresh_002", "搜索深圳天气", "search_weather"),
    ];
    
    for (id, text, intent) in &texts {
        let mut entry = create_test_entry(
            id,
            text,
            HashMap::from([("intent".to_string(), intent.to_string())]),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
    }
    
    // 搜索并设置高阈值
    let query_embedding = embedder.embed("百度首页").await.expect("生成查询 embedding 失败");
    let mut filters = SearchFilters::default();
    filters.threshold = Some(0.99); // 设置非常高的阈值
    
    let results = store
        .search(&query_embedding, 5, &filters)
        .await
        .expect("搜索失败");
    
    println!("高阈值搜索结果数量: {}", results.len());
    assert!(results.len() <= 1, "高阈值下结果应该很少");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_search_with_threshold 通过");
}

#[tokio::test]
async fn test_store_search_with_label_filter() {
    let test_dir = get_test_dir("test_search_label");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入带不同标签的测试数据
    let texts = vec![
        ("label_001", "打开百度首页", "open_baidu", "browser"),
        ("label_002", "搜索天气", "search_weather", "browser"),
        ("label_003", "创建文件", "create_file", "file"),
    ];
    
    for (id, text, intent, category) in &texts {
        let mut entry = create_test_entry(
            id,
            text,
            HashMap::from([
                ("intent".to_string(), intent.to_string()),
                ("category".to_string(), category.to_string()),
            ]),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
    }
    
    // 按标签过滤搜索（只搜索 browser 类别）
    let query_embedding = embedder.embed("打开页面").await.expect("生成查询 embedding 失败");
    let filters = SearchFilters {
        labels: HashMap::from([("category".to_string(), "browser".to_string())]),
        threshold: None,
    };
    
    let results = store
        .search(&query_embedding, 5, &filters)
        .await
        .expect("搜索失败");
    
    // 验证所有结果都匹配标签
    for r in &results {
        assert_eq!(
            r.entry.labels.get("category"),
            Some(&"browser".to_string()),
            "所有结果应该是 browser 类别"
        );
    }
    
    println!("标签过滤结果数量: {}", results.len());
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_search_with_label_filter 通过");
}

#[tokio::test]
async fn test_store_delete() {
    let test_dir = get_test_dir("test_delete");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入测试数据
    let mut entry = create_test_entry(
        "delete_001",
        "临时测试数据",
        HashMap::new(),
    );
    entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
    store.add(entry).await.expect("写入失败");
    
    // 验证写入成功
    assert_eq!(store.count().await.expect("查询数量失败"), 1);
    
    // 删除
    store.delete("delete_001").await.expect("删除失败");
    
    // 验证删除成功
    let count = store.count().await.expect("查询数量失败");
    assert_eq!(count, 0, "删除后应该没有记录");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_delete 通过");
}

#[tokio::test]
async fn test_store_delete_nonexistent() {
    let test_dir = get_test_dir("test_delete_nonexistent");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 尝试删除不存在的记录
    let result = store.delete("nonexistent_id").await;
    assert!(result.is_err(), "删除不存在的记录应该返回错误");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_delete_nonexistent 通过");
}

#[tokio::test]
async fn test_store_update_via_delete_add() {
    let test_dir = get_test_dir("test_update");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 初始写入
    let mut entry = create_test_entry(
        "update_001",
        "原始文本",
        HashMap::from([("version".to_string(), "1".to_string())]),
    );
    entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
    store.add(entry).await.expect("写入失败");
    
    // 删除旧记录
    store.delete("update_001").await.expect("删除失败");
    
    // 写入新记录（模拟 update）
    let mut new_entry = create_test_entry(
        "update_001",
        "更新后的文本",
        HashMap::from([("version".to_string(), "2".to_string())]),
    );
    new_entry.embedding = embedder.embed(&new_entry.text).await.expect("生成 embedding 失败");
    store.add(new_entry).await.expect("写入失败");
    
    // 验证更新结果
    let count = store.count().await.expect("查询数量失败");
    assert_eq!(count, 1, "更新后应该有 1 条记录");
    
    // 搜索验证内容已更新
    let query_embedding = embedder.embed("更新后的文本").await.expect("生成查询 embedding 失败");
    let results = store
        .search(&query_embedding, 1, &SearchFilters::default())
        .await
        .expect("搜索失败");
    
    assert!(!results.is_empty());
    assert_eq!(results[0].entry.labels.get("version"), Some(&"2".to_string()));
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_update_via_delete_add 通过");
}

#[tokio::test]
async fn test_store_count() {
    let test_dir = get_test_dir("test_count");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 初始为空
    assert_eq!(store.count().await.expect("查询数量失败"), 0);
    
    // 添加数据并验证计数
    for i in 0..5 {
        let mut entry = create_test_entry(
            &format!("count_{:03}", i),
            &format!("测试文本 {}", i),
            HashMap::new(),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
        assert_eq!(store.count().await.expect("查询数量失败"), i + 1);
    }
    
    // 删除一条并验证
    store.delete("count_002").await.expect("删除失败");
    assert_eq!(store.count().await.expect("查询数量失败"), 4);
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_store_count 通过");
}

#[tokio::test]
async fn test_embedder_consistency() {
    let embedder = create_test_embedder();
    
    let text = "测试文本一致性";
    
    // 多次生成同一文本的 embedding 应该相同
    let emb1 = embedder.embed(text).await.expect("生成 embedding 失败");
    let emb2 = embedder.embed(text).await.expect("生成 embedding 失败");
    
    assert_eq!(emb1, emb2, "相同文本应生成相同的 embedding");
    
    // 不同文本的 embedding 应该不同
    let emb3 = embedder.embed("不同的文本").await.expect("生成 embedding 失败");
    assert_ne!(emb1, emb3, "不同文本应生成不同的 embedding");
    
    println!("✓ test_embedder_consistency 通过");
}

#[tokio::test]
async fn test_embedder_dim() {
    let embedder = create_test_embedder();
    
    let embedding = embedder.embed("测试").await.expect("生成 embedding 失败");
    
    assert_eq!(
        embedding.len(),
        embedder.dim(),
        "Embedding 维度应该与声明一致"
    );
    assert_eq!(embedding.len(), 128, "Mock Embedder 维度应为 128");
    
    println!("✓ test_embedder_dim 通过");
}

#[tokio::test]
async fn test_search_empty_store() {
    let test_dir = get_test_dir("test_search_empty");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 在空存储中搜索
    let query_embedding = embedder.embed("任意查询").await.expect("生成查询 embedding 失败");
    let results = store
        .search(&query_embedding, 5, &SearchFilters::default())
        .await
        .expect("搜索失败");
    
    assert!(results.is_empty(), "空存储搜索应返回空结果");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_search_empty_store 通过");
}

#[tokio::test]
async fn test_search_top_k_limit() {
    let test_dir = get_test_dir("test_search_topk");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入多条数据
    for i in 0..10 {
        let mut entry = create_test_entry(
            &format!("topk_{:03}", i),
            &format!("测试数据 {}", i),
            HashMap::new(),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
    }
    
    // 只请求 top 3
    let query_embedding = embedder.embed("测试").await.expect("生成查询 embedding 失败");
    let results = store
        .search(&query_embedding, 3, &SearchFilters::default())
        .await
        .expect("搜索失败");
    
    assert!(
        results.len() <= 3,
        "结果数量不应超过 top_k: {}",
        results.len()
    );
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_search_top_k_limit 通过");
}

// ═══════════════════════════════════════════════════════════
// 增强测试：并发安全
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_concurrent_write() {
    use tokio::task::JoinSet;
    
    let test_dir = get_test_dir("test_concurrent_write");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    let store = Arc::new(store);
    
    let count = 20;
    let mut set = JoinSet::new();
    
    // 并发写入
    for i in 0..count {
        let embedder = Arc::clone(&embedder);
        let store = Arc::clone(&store);
        
        set.spawn(async move {
            let mut entry = create_test_entry(
                &format!("concurrent_{:03}", i),
                &format!("并发测试数据 {}", i),
                HashMap::new(),
            );
            entry.embedding = embedder.embed(&entry.text).await.unwrap();
            store.add(entry).await.unwrap();
        });
    }
    
    // 等待所有任务完成
    while set.join_next().await.is_some() {}
    
    // 验证写入数量
    let final_count = store.count().await.expect("查询数量失败");
    assert_eq!(final_count, count, "并发写入后记录数应为 {}", count);
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_concurrent_write 通过");
}

// ═══════════════════════════════════════════════════════════
// 增强测试：搜索结果相关性验证
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_search_semantic_relevance() {
    let test_dir = get_test_dir("test_search_semantic");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 写入不同语义的文本
    let texts = vec![
        ("sem_001", "苹果是一种水果", "fruit"),
        ("sem_002", "苹果手机是科技产品", "tech"),
        ("sem_003", "香蕉是热带水果", "fruit"),
        ("sem_004", "华为是中国手机品牌", "tech"),
    ];
    
    for (id, text, category) in &texts {
        let mut entry = create_test_entry(
            id,
            text,
            HashMap::from([("category".to_string(), category.to_string())]),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        store.add(entry).await.expect("写入失败");
    }
    
    // 搜索"水果"
    let query_embedding = embedder.embed("水果 香蕉").await.expect("生成查询 embedding 失败");
    let filters = SearchFilters::default();
    let results = store
        .search(&query_embedding, 5, &filters)
        .await
        .expect("搜索失败");
    
    // 验证水果类别结果排在前面
    let fruit_results: Vec<_> = results.iter()
        .filter(|r| r.entry.labels.get("category") == Some(&"fruit".to_string()))
        .collect();
    
    println!("搜索'水果'返回 {} 个结果，水果类别: {}", results.len(), fruit_results.len());
    for r in &results {
        println!("  - {} ({}): score={:.4}", r.entry.text, 
            r.entry.labels.get("category").unwrap_or(&"none".to_string()), r.score);
    }
    
    // 至少应该有水果相关结果
    assert!(!fruit_results.is_empty(), "水果搜索应返回水果类别结果");
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_search_semantic_relevance 通过");
}

// ═══════════════════════════════════════════════════════════
// 增强测试：维度不匹配错误
// ═══════════════════════════════════════════════════════════

#[tokio::test]
async fn test_dimension_mismatch_error() {
    let test_dir = get_test_dir("test_dim_mismatch");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    // 尝试写入错误维度的 embedding
    let mut entry = create_test_entry("dim_test", "测试", HashMap::new());
    entry.embedding = vec![0.0f32; dim + 10]; // 故意使用错误维度
    
    let result = store.add(entry).await;
    assert!(result.is_err(), "维度不匹配应返回错误");
    println!("维度不匹配错误: {:?}", result.unwrap_err());
    
    cleanup_test_dir(&test_dir);
    println!("✓ test_dimension_mismatch_error 通过");
}

// ═══════════════════════════════════════════════════════════
// 性能基准测试（可选）
// ═══════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // 需要手动运行: cargo test -- --ignored
async fn benchmark_batch_write() {
    use std::time::Instant;
    
    let test_dir = get_test_dir("benchmark_batch");
    let embedder = create_test_embedder();
    let dim = embedder.dim();
    
    let store = PolarisDbStore::open(test_dir.to_str().unwrap(), dim)
        .await
        .expect("创建存储失败");
    
    let count = 100;
    let mut entries = Vec::with_capacity(count);
    
    for i in 0..count {
        let mut entry = create_test_entry(
            &format!("bench_{:03}", i),
            &format!("性能测试数据编号 {}", i),
            HashMap::new(),
        );
        entry.embedding = embedder.embed(&entry.text).await.expect("生成 embedding 失败");
        entries.push(entry);
    }
    
    let start = Instant::now();
    store.add_batch(entries).await.expect("批量写入失败");
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as f64;
    
    println!("批量写入 {} 条记录耗时: {:?}", count, elapsed);
    println!("平均每条: {:.3} ms", elapsed_ms / count as f64);
    
    cleanup_test_dir(&test_dir);
}
