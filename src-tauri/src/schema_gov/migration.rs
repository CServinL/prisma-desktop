//! Versioned-migration chain -- the Rust twin of prisma's own
//! `schema_gov.VersionedModel` (Python): a `schema_version` field, an
//! absent-version-means-v1 convention (the shape data had before this
//! mechanism was ever applied to it), and a migration chain applied to the
//! raw JSON before deserializing into a concrete struct.

use std::collections::HashMap;

use serde_json::Value;

pub type MigrationFn = fn(Value) -> Result<Value, String>;

/// A chain of migration functions, keyed by the version being upgraded
/// *from* -- e.g. `{1: v1_to_v2}` upgrades a v1 shape to v2. Never rewrite
/// an existing step once shipped -- each version's upgrade path must stay
/// correct for data frozen at that version, however old.
pub struct MigrationChain {
    pub current_version: u32,
    pub migrations: HashMap<u32, MigrationFn>,
}

impl MigrationChain {
    pub fn migrate(&self, raw: Value) -> Result<Value, String> {
        let version = raw
            .as_object()
            .ok_or_else(|| "expected a JSON object".to_string())?
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;

        if version > self.current_version {
            return Err(format!(
                "schema_version {version} is newer than this build supports ({})",
                self.current_version
            ));
        }

        let mut value = raw;
        let mut v = version;
        while v < self.current_version {
            let migrate = self
                .migrations
                .get(&v)
                .ok_or_else(|| format!("no migration registered from schema_version {v}"))?;
            value = migrate(value)?;
            v += 1;
        }

        if let Value::Object(map) = &mut value {
            map.insert("schema_version".to_string(), Value::from(self.current_version));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_schema_version_is_treated_as_v1() {
        let chain = MigrationChain { current_version: 1, migrations: HashMap::new() };
        let result = chain.migrate(json!({"name": "gadget"})).unwrap();
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["name"], "gadget");
    }

    #[test]
    fn current_version_passes_through_unchanged() {
        let chain = MigrationChain { current_version: 1, migrations: HashMap::new() };
        let result = chain.migrate(json!({"schema_version": 1, "name": "gadget"})).unwrap();
        assert_eq!(result["schema_version"], 1);
    }

    #[test]
    fn errors_for_a_version_newer_than_this_build_supports() {
        let chain = MigrationChain { current_version: 1, migrations: HashMap::new() };
        let err = chain.migrate(json!({"schema_version": 99})).unwrap_err();
        assert!(err.contains("newer than this build supports"), "{err}");
    }

    fn v1_to_v2(mut raw: Value) -> Result<Value, String> {
        let obj = raw.as_object_mut().ok_or("expected object")?;
        if let Some(name) = obj.remove("name") {
            obj.insert("label".to_string(), name);
        }
        Ok(raw)
    }

    #[test]
    fn migration_chain_upgrades_an_old_shape() {
        let mut migrations: HashMap<u32, MigrationFn> = HashMap::new();
        migrations.insert(1, v1_to_v2);
        let chain = MigrationChain { current_version: 2, migrations };
        let result = chain.migrate(json!({"name": "gadget"})).unwrap();
        assert_eq!(result["label"], "gadget");
        assert_eq!(result["schema_version"], 2);
    }

    #[test]
    fn missing_migration_step_errors() {
        let chain = MigrationChain { current_version: 2, migrations: HashMap::new() };
        let err = chain.migrate(json!({"schema_version": 1})).unwrap_err();
        assert!(err.contains("no migration registered"), "{err}");
    }
}
