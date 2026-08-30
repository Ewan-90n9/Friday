use serde_json::{json, Value};

use super::HeapToolKind;

/// Friday heap 工具参数 → 上游 jvm-heap-dump-mcp 工具名 + 参数。Err(String) → invalid_params。
pub fn build(kind: HeapToolKind, args: &Value) -> Result<(String, Value), String> {
    match kind {
        HeapToolKind::Open | HeapToolKind::Close => Err("内部错误：open/close 不经 mapping".into()),
        HeapToolKind::LeakSuspects => Ok(("get_leak_suspects".into(), json!({}))),
        HeapToolKind::Histogram => {
            let limit = limit_arg(args, "top", 30)?;
            let sort_by = match args.get("sort_by").and_then(|v| v.as_str()) {
                None | Some("retained_heap") => "RETAINED_HEAP",
                Some("shallow_heap") => "SHALLOW_HEAP",
                Some("objects") => "OBJECTS",
                Some(other) => {
                    return Err(format!("sort_by 非法: {other}（可选 retained_heap / shallow_heap / objects）"))
                }
            };
            let mut a = json!({ "limit": limit, "sortBy": sort_by });
            if let Some(f) = args.get("filter").and_then(|v| v.as_str()) {
                a["filter"] = json!(f);
            }
            Ok(("get_class_histogram".into(), a))
        }
        HeapToolKind::DominatorTree => {
            let limit = limit_arg(args, "top", 30)?;
            match optional_object_id(args, "parent_object_id")? {
                None => Ok(("get_dominator_tree".into(), json!({ "limit": limit }))),
                Some(oid) => Ok(("get_dominator_tree_children".into(), json!({ "objectId": oid, "limit": limit }))),
            }
        }
        HeapToolKind::ObjectInfo => Ok(("get_object_info".into(), json!({ "objectId": object_id(args)? }))),
        HeapToolKind::PathToGcRoots => {
            Ok(("get_path_to_gc_roots".into(), json!({ "objectId": object_id(args)? })))
        }
        HeapToolKind::References => {
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: direction（outbound / inbound）")?;
            let upstream = match direction {
                "outbound" => "get_outbound_references",
                "inbound" => "get_inbound_references",
                other => return Err(format!("direction 非法: {other}（可选 outbound / inbound）")),
            };
            Ok((
                upstream.into(),
                json!({ "objectId": object_id(args)?, "limit": limit_arg(args, "top", 50)? }),
            ))
        }
        HeapToolKind::Threads => {
            let mut a = json!({});
            if let Some(f) = args.get("filter").and_then(|v| v.as_str()) {
                a["filter"] = json!(f);
            }
            Ok(("get_threads".into(), a))
        }
    }
}

fn object_id(args: &Value) -> Result<i64, String> {
    let n = args
        .get("object_id")
        .and_then(|v| v.as_i64())
        .ok_or("missing required parameter: object_id（正整数，来自 heap_dominator_tree / heap_histogram / heap_references 结果）")?;
    if n <= 0 {
        return Err("object_id 必须是正整数".into());
    }
    Ok(n)
}

fn optional_object_id(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| format!("{key} 必须是正整数"))?;
            if n <= 0 {
                return Err(format!("{key} 必须是正整数"));
            }
            Ok(Some(n))
        }
    }
}

fn limit_arg(args: &Value, key: &str, default: i64) -> Result<i64, String> {
    match args.get(key).and_then(|v| v.as_i64()) {
        None => Ok(default),
        Some(n) if (1..=200).contains(&n) => Ok(n),
        Some(n) => Err(format!("{key} 必须在 1..=200 之间，收到 {n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_histogram_defaults_and_overrides() {
        let (name, args) = build(HeapToolKind::Histogram, &json!({})).unwrap();
        assert_eq!(name, "get_class_histogram");
        assert_eq!(args["limit"], 30);
        assert_eq!(args["sortBy"], "RETAINED_HEAP");

        let (_, args) = build(
            HeapToolKind::Histogram,
            &json!({"top": 5, "sort_by": "shallow_heap", "filter": "com\\.example\\."}),
        )
        .unwrap();
        assert_eq!(args["limit"], 5);
        assert_eq!(args["sortBy"], "SHALLOW_HEAP");
        assert_eq!(args["filter"], "com\\.example\\.");
    }

    #[test]
    fn test_histogram_rejects_bad_sort_and_limit() {
        assert!(build(HeapToolKind::Histogram, &json!({"sort_by": "bogus"})).is_err());
        assert!(build(HeapToolKind::Histogram, &json!({"top": 0})).is_err());
        assert!(build(HeapToolKind::Histogram, &json!({"top": 999})).is_err());
    }

    #[test]
    fn test_dominator_tree_root_vs_children() {
        let (name, args) = build(HeapToolKind::DominatorTree, &json!({})).unwrap();
        assert_eq!(name, "get_dominator_tree");
        assert_eq!(args["limit"], 30);

        let (name, args) =
            build(HeapToolKind::DominatorTree, &json!({"parent_object_id": 42, "top": 10})).unwrap();
        assert_eq!(name, "get_dominator_tree_children");
        assert_eq!(args["objectId"], 42);
        assert_eq!(args["limit"], 10);

        assert!(build(HeapToolKind::DominatorTree, &json!({"parent_object_id": -1})).is_err());
    }

    #[test]
    fn test_object_id_required_positive() {
        assert!(build(HeapToolKind::ObjectInfo, &json!({})).is_err());
        assert!(build(HeapToolKind::ObjectInfo, &json!({"object_id": -1})).is_err());
        let (_, args) = build(HeapToolKind::ObjectInfo, &json!({"object_id": 7})).unwrap();
        assert_eq!(args["objectId"], 7);
    }

    #[test]
    fn test_references_direction() {
        let (name, args) =
            build(HeapToolKind::References, &json!({"object_id": 9, "direction": "inbound"})).unwrap();
        assert_eq!(name, "get_inbound_references");
        assert_eq!(args["objectId"], 9);
        assert_eq!(args["limit"], 50);

        let (name, _) =
            build(HeapToolKind::References, &json!({"object_id": 9, "direction": "outbound"})).unwrap();
        assert_eq!(name, "get_outbound_references");

        assert!(build(HeapToolKind::References, &json!({"object_id": 9})).is_err());
        assert!(build(HeapToolKind::References, &json!({"object_id": 9, "direction": "sideways"})).is_err());
    }

    #[test]
    fn test_threads_filter_passthrough() {
        let (name, args) = build(HeapToolKind::Threads, &json!({"filter": "http-nio"})).unwrap();
        assert_eq!(name, "get_threads");
        assert_eq!(args["filter"], "http-nio");
    }

    #[test]
    fn test_leak_suspects_no_extra_args() {
        let (name, args) = build(HeapToolKind::LeakSuspects, &json!({})).unwrap();
        assert_eq!(name, "get_leak_suspects");
        assert!(args.as_object().unwrap().is_empty());
    }
}
