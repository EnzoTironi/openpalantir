//! Funnel ingest rules. Undeclared fields never become Mapped.

use crate::error::{OntoError, Result};
use crate::types::{ObjectTypeSpec, PropertySpec};

/// Return the published property, or fail closed on an unknown field.
pub fn declared_property<'a>(spec: &'a ObjectTypeSpec, name: &str) -> Result<&'a PropertySpec> {
    spec.properties
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| OntoError::Invalid(format!("undeclared field {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PropertySource, Typology};

    #[test]
    fn does_reject_undeclared_field() {
        let spec = ObjectTypeSpec {
            name: "Gauge".into(),
            typology: Typology::Entity,
            title_prop: None,
            interfaces: vec![],
            freshness_budget_secs: None,
            properties: vec![],
        };
        let err = declared_property(&spec, "secret").unwrap_err();
        assert!(matches!(err, OntoError::Invalid(msg) if msg.contains("undeclared")));
    }
}
