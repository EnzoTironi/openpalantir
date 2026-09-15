//! Buyer usefulness scoreboard: claim → named `does_*` test → PASS/FAIL.
//!
//! Not a latency bench. Each row is an operational-ontology capability
//! proven by a behavioral test. Cite Zhang 2026; do not copy the book.

/// One buyer claim pinned to a declarative test name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub claim: &'static str,
    pub test: &'static str,
}

/// Scoreboard rows. Test names must exist as `fn does_*`.
pub const CLAIMS: &[Claim] = &[
    Claim {
        claim: "OMS vivo: tipos kernel são ObjectType consultáveis",
        test: "does_expose_kernel_types_as_queryable_objecttype_records",
    },
    Claim {
        claim: "Schema em branch não altera produção até o merge",
        test: "does_hide_branch_alter_until_merge",
    },
    Claim {
        claim: "Chave builder não lê instância; consumer não edita schema",
        test: "does_isolate_builder_and_consumer_keys",
    },
    Claim {
        claim: "Escrita de negócio percorre os 7 passos do write path",
        test: "does_walk_write_path_seven_steps_in_order",
    },
    Claim {
        claim: "Falha de guarda no critério descarta o stage",
        test: "does_discard_stage_if_guard_fails_at_criteria",
    },
    Claim {
        claim: "Reviewable em Propose cria inbox; attach não mesclado é no-op",
        test: "does_create_inbox_if_reviewable_and_ignore_unmerged_attach",
    },
    Claim {
        claim: "Evidenced: evidência ausente é Review, não Allow",
        test: "does_review_if_evidenced_submit_lacks_evidence",
    },
    Claim {
        claim: "Funnel não sobrescreve propriedade ActionWritten",
        test: "does_keep_action_written_target_do_if_funnel_ingests",
    },
    Claim {
        claim: "create_link não é escrita pública; só Action ou Funnel",
        test: "does_refuse_create_link_on_public_dispatch",
    },
    Claim {
        claim: "Sem grant é Deny (plataforma, tipo, instância, propriedade)",
        test: "does_deny_if_grant_is_missing",
    },
    Claim {
        claim: "Propriedade Deny some em get_object e object sets",
        test: "does_hide_denied_properties_on_get_and_sets_if_restricted",
    },
    Claim {
        claim: "Object set vazio é Ok([]), não erro",
        test: "does_return_empty_vec_if_object_set_is_empty",
    },
    Claim {
        claim: "Escritas acrescentam versão; as_of lê o histórico",
        test: "does_append_versions_and_read_history_as_of",
    },
    Claim {
        claim: "Compensar Allow é Action inversa, não rollback",
        test: "does_compensate_allow_as_inverse_action_not_rollback",
    },
    Claim {
        claim: "Compensação sem nome é erro tipado, não sucesso silencioso",
        test: "does_error_if_compensation_name_is_missing",
    },
    Claim {
        claim: "T1 observa e não submete Action",
        test: "does_let_t1_observe_and_refuse_submit",
    },
    Claim {
        claim: "T3 confirma outro ator, não a si",
        test: "does_let_t3_confirm_other_actor_not_self",
    },
    Claim {
        claim: "T4 auto recusado se o bound está vazio",
        test: "does_deny_t4_auto_if_bound_is_empty",
    },
    Claim {
        claim: "Sensor stale: Review e nomeia request_sensor_calibration",
        test: "does_review_and_name_calibration_if_sensor_is_stale",
    },
    Claim {
        claim: "Setpoint acima do permit é Deny",
        test: "does_deny_if_target_do_exceeds_permit",
    },
    Claim {
        claim: "Turma cheia: segunda confirmação no mesmo assento é Deny",
        test: "does_deny_second_confirm_if_seat_already_occupied",
    },
    Claim {
        claim: "Healthcare: observação ausente é Review, não Allow",
        test: "does_review_and_not_allow_if_observation_is_missing",
    },
    Claim {
        claim: "Healthcare instrucional: sem calculadora de quantidade prescrita",
        test: "does_omit_prescribed_quantity_calculator",
    },
    Claim {
        claim: "Função registrada no OMS vale só depois do merge",
        test: "does_evaluate_runtime_function_after_merge",
    },
    Claim {
        claim: "DecisionRecord pina objetos lidos e versões de regra/função/motor",
        test: "does_pin_reads_and_versions_on_decision_snapshot",
    },
    Claim {
        claim: "Motor não fala SQL; Store coordena a persistência",
        test: "does_keep_engine_free_of_sql",
    },
    Claim {
        claim: "Store é o único trait público de persistência",
        test: "does_keep_store_as_only_persistence_trait",
    },
    Claim {
        claim: "Consumer MCP não projeta create_object_type nem create_link",
        test: "does_project_consumer_tools_from_live_oms",
    },
];

/// PASS only when the named test ran and succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
}

impl Outcome {
    #[must_use]
    pub fn as_cell(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
        }
    }
}

/// Read `cargo test` lines (`test name ... ok|FAILED`). Missing name is Fail.
#[must_use]
pub fn outcome_for(test: &str, cargo_output: &str) -> Outcome {
    let mut saw_ok = false;
    let mut saw_fail = false;
    for line in cargo_output.lines() {
        let Some((name, status)) = parse_result_line(line) else {
            continue;
        };
        if !name_matches(name, test) {
            continue;
        }
        match status {
            "ok" => saw_ok = true,
            "FAILED" => saw_fail = true,
            _ => {}
        }
    }
    if saw_fail || !saw_ok {
        Outcome::Fail
    } else {
        Outcome::Pass
    }
}

fn parse_result_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("test ")?;
    let (name, tail) = rest.split_once(" ... ")?;
    let status = tail.split_whitespace().next()?;
    Some((name.trim(), status))
}

fn name_matches(full: &str, test: &str) -> bool {
    full == test || full.rsplit("::").next() == Some(test)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_mark_pass_if_named_test_ok() {
        let out = "test does_walk_write_path_seven_steps_in_order ... ok\n";
        assert_eq!(
            outcome_for("does_walk_write_path_seven_steps_in_order", out),
            Outcome::Pass
        );
    }

    #[test]
    fn does_mark_fail_if_named_test_failed() {
        let out = "test onto::write_path::tests::does_keep_action_written_target_do_if_funnel_ingests ... FAILED\n";
        assert_eq!(
            outcome_for("does_keep_action_written_target_do_if_funnel_ingests", out),
            Outcome::Fail
        );
    }

    #[test]
    fn does_mark_fail_if_named_test_is_absent() {
        let out = "test does_other ... ok\n";
        assert_eq!(
            outcome_for("does_walk_write_path_seven_steps_in_order", out),
            Outcome::Fail
        );
    }

    #[test]
    fn does_name_every_claim_as_does_test() {
        assert!(
            CLAIMS.len() >= 20,
            "buyer scoreboard must stay comprehensive"
        );
        for row in CLAIMS {
            assert!(
                row.test.starts_with("does_"),
                "scoreboard must pin a does_* test, got {}",
                row.test
            );
            assert!(!row.claim.is_empty());
        }
    }
}
