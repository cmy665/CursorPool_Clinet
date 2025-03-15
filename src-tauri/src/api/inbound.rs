use crate::database::Database;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

// 线路配置数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundConfig {
    pub inbound: Vec<InboundItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundItem {
    pub name: String,
    pub url: String,
}

// 带延迟信息的线路条目
#[derive(Debug, Clone)]
struct InboundItemWithLatency {
    index: usize,
    item: InboundItem,
    latency: Option<Duration>,
}

// 默认URL
const DEFAULT_INBOUND_URL: &str = "https://pool.52ai.org";
// 配置文件URL
const CONFIG_URL: &str = "https://cursorpool.oss-cn-guangzhou.aliyuncs.com/config.json";
// 数据库键名
const INBOUND_CONFIG_KEY: &str = "system.inbound.config";
const CURRENT_INBOUND_KEY: &str = "system.inbound.current";
// 测速超时时间（毫秒）
const PING_TIMEOUT_MS: u64 = 5000;

/// 从远程获取线路配置
pub async fn fetch_inbound_config() -> Result<InboundConfig, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("创建HTTP客户端失败: {}", e))?;

    let response = client
        .get(CONFIG_URL)
        .send()
        .await
        .map_err(|e| format!("请求线路配置失败: {}", e))?;

    let config = response
        .json::<InboundConfig>()
        .await
        .map_err(|e| format!("解析线路配置失败: {}", e))?;

    if config.inbound.is_empty() {
        return Err("线路配置为空".to_string());
    }

    Ok(config)
}

/// 测试单个线路的延迟
async fn test_inbound_latency(url: &str) -> Option<Duration> {
    let client = Client::builder()
        .timeout(Duration::from_millis(PING_TIMEOUT_MS))
        .build()
        .ok()?;
    
    // 测试版本信息接口（通常是轻量级的）
    let test_url = format!("{}/version", url);
    let start = Instant::now();
    
    match client.get(&test_url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                Some(start.elapsed())
            } else {
                None // 服务器返回错误
            }
        },
        Err(_) => None // 请求失败（超时或网络错误）
    }
}

/// 测试所有线路并返回延迟最低的有效线路索引
async fn find_fastest_inbound(config: &InboundConfig) -> usize {
    // 如果只有一个线路，直接返回
    if config.inbound.len() == 1 {
        return 0;
    }
    
    let mut items_with_latency = Vec::new();
    
    // 测试所有线路
    for (index, item) in config.inbound.iter().enumerate() {
        let api_base_url = format!("{}/api", item.url);
        let latency = test_inbound_latency(&api_base_url).await;
        
        items_with_latency.push(InboundItemWithLatency {
            index,
            item: item.clone(),
            latency,
        });
        
        // 打印测试结果
        match latency {
            Some(duration) => println!("线路 [{}] {} 延迟: {:?}", index, item.name, duration),
            None => println!("线路 [{}] {} 不可用", index, item.name),
        }
    }
    
    // 筛选可用的线路
    let available_items: Vec<_> = items_with_latency.iter()
        .filter(|item| item.latency.is_some())
        .collect();
    
    if available_items.is_empty() {
        println!("所有线路均不可用，使用第一个线路");
        return 0;
    }
    
    // 找到延迟最低的线路
    let fastest = available_items.iter()
        .min_by_key(|item| item.latency.unwrap())
        .unwrap();
    
    println!("选择延迟最低的线路 [{}] {}: {:?}", 
             fastest.index, fastest.item.name, fastest.latency.unwrap());
    
    fastest.index
}

/// 初始化线路配置
pub async fn init_inbound_config(app: &AppHandle) -> Result<(), String> {
    let db = app.state::<Database>();
    
    // 尝试获取远程线路配置
    let config_result = fetch_inbound_config().await;
    
    match config_result {
        Ok(config) => {
            // 将配置保存到数据库
            let config_json = serde_json::to_string(&config)
                .map_err(|e| format!("序列化线路配置失败: {}", e))?;
            
            db.set_item(INBOUND_CONFIG_KEY, &config_json)
                .map_err(|e| format!("保存线路配置失败: {}", e))?;
            
            // 检查当前是否已有选择的线路
            if let Ok(None) = db.get_item(CURRENT_INBOUND_KEY) {
                // 如果没有，进行测速选择最佳线路
                println!("未找到已选择线路，开始测速选择最佳线路...");
                let best_index = find_fastest_inbound(&config).await;
                
                // 保存最佳线路
                db.set_item(CURRENT_INBOUND_KEY, &best_index.to_string())
                    .map_err(|e| format!("设置当前线路失败: {}", e))?;
                
                println!("已选择最佳线路：[{}] {}", best_index, config.inbound[best_index].name);
            }
            
            println!("线路配置初始化成功，共{}条线路", config.inbound.len());
        },
        Err(e) => {
            println!("获取远程线路配置失败: {}，将使用默认线路", e);
            
            // 检查数据库中是否已有配置
            if let Ok(None) = db.get_item(INBOUND_CONFIG_KEY) {
                // 创建默认配置
                let default_config = InboundConfig {
                    inbound: vec![InboundItem {
                        name: "默认线路".to_string(),
                        url: DEFAULT_INBOUND_URL.to_string(),
                    }],
                };
                
                let config_json = serde_json::to_string(&default_config)
                    .map_err(|e| format!("序列化默认线路配置失败: {}", e))?;
                
                db.set_item(INBOUND_CONFIG_KEY, &config_json)
                    .map_err(|e| format!("保存默认线路配置失败: {}", e))?;
                
                db.set_item(CURRENT_INBOUND_KEY, "0")
                    .map_err(|e| format!("设置当前线路失败: {}", e))?;
                
                println!("已创建默认线路配置");
            }
        }
    }
    
    Ok(())
}

/// 获取当前线路URL
pub fn get_current_inbound_url(db: &Database) -> String {
    // 获取当前选择的线路索引
    let current_index = match db.get_item(CURRENT_INBOUND_KEY) {
        Ok(Some(index)) => index.parse::<usize>().unwrap_or(0),
        _ => 0,
    };
    
    // 获取线路配置
    let config = match db.get_item(INBOUND_CONFIG_KEY) {
        Ok(Some(config_json)) => {
            match serde_json::from_str::<InboundConfig>(&config_json) {
                Ok(config) => config,
                Err(e) => {
                    println!("解析线路配置失败: {}", e);
                    return format!("{}/api", DEFAULT_INBOUND_URL);
                }
            }
        },
        _ => {
            return format!("{}/api", DEFAULT_INBOUND_URL);
        }
    };
    
    // 获取当前线路URL
    if let Some(inbound) = config.inbound.get(current_index) {
        format!("{}/api", inbound.url)
    } else {
        format!("{}/api", DEFAULT_INBOUND_URL)
    }
} 