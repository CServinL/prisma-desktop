//! Thin `jsonschema`-crate wrapper -- validates a JSON instance against a
//! JSON Schema (the same schema Python's `schema_gov.export_schemas`
//! produces via Pydantic's `model_json_schema()`).

use jsonschema::validator_for;
use serde_json::Value;

pub fn validate_against_schema(schema: &Value, instance: &Value) -> Result<(), Vec<String>> {
    let validator = validator_for(schema).map_err(|e| vec![e.to_string()])?;
    let errors: Vec<String> = validator.iter_errors(instance).map(|e| e.to_string()).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        })
    }

    #[test]
    fn valid_instance_passes() {
        assert!(validate_against_schema(&schema(), &json!({"name": "gadget"})).is_ok());
    }

    #[test]
    fn missing_required_field_fails_with_an_error() {
        let errors = validate_against_schema(&schema(), &json!({})).unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn wrong_type_fails_with_an_error() {
        let errors = validate_against_schema(&schema(), &json!({"name": 123})).unwrap_err();
        assert!(!errors.is_empty());
    }
}
