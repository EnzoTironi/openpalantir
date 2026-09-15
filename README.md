# Operational Ontology OS

Runtime de ontologia operacional em Rust: OMS vivo, object store, OSS, Actions, Functions e Funnel. Sem UI. A interface é MCP (e o SDK/CLI que falam a mesma superfície).

Inspirado em Zhang, *Operational Ontology: From Business Mirror to Decision Runtime* (First Public Edition, v1.0, 2026). Código original. Não é um produto Palantir e não usa nomes de produto na API pública.

## Regras

- A constituição mora no **OMS**. Tipos de domínio nascem em runtime (`open_branch` → create/alter → `submit_proposal` → `review_proposal` → `merge_to_main`).
- Só o meta-modelo é compilado: `ObjectType`, `PropertyType`, `ValueType`, `LinkType`, `InterfaceType`, `ActionType`, `Policy`, `OntologyBranch`, `OntologyProposal`.
- **Chave consumer:** consultar o mundo e executar Actions pré-definidas. Não edita schema.
- **Chave builder:** edita schema num branch. Não lê nem escreve instâncias de produção.
- Toda escrita de negócio passa por Action no write path de 7 passos (`WritePathStep`): submit → param+permission → submission criteria → staged edits (all-or-discard) → commit atômico → selar `DecisionRecord` → declarar side effects com chave de idempotência. Funnel não sobrescreve propriedade `ActionWritten`.
- `DecisionRecord.data_snapshot` pina os objetos lidos nos guards, mais `rule_version`, `function_version` e `engine_version`.
- Inbox é objeto, não tela. Confirmar/vetar é Action.
- **OSS:** `ObjectSet` é spec tipada (tipo + filtro + nome opcional), não uma lista de ids. `search_objects` avalia o conjunto depois do filtro de permissão. Conjuntos nomeados nascem num branch de builder (`create_object_set`) e só aparecem no `main` depois do merge. Conjunto vazio é `Ok([])`. `aggregate` conta o conjunto filtrado.

## Crates

- `crates/onto` — motor
- `crates/onto-sdk` — cliente + CLI `onto`
- `crates/onto-mcp` — MCP stdio e HTTP em `:43177/mcp`
- `crates/onto-bootstrap` — caso wastewater via as mesmas APIs de builder

## Rodar

```bash
cargo test --workspace -- --test-threads=1
```

MCP consumer (stdio):

```bash
cargo run -p onto-mcp -- --key consumer --bootstrap
```

MCP builder:

```bash
cargo run -p onto-mcp -- --key builder --db onto.db
```

HTTP (porta 43177):

```bash
cargo run -p onto-mcp -- --key consumer --bootstrap --http
```

CLI de debug (não é produto):

```bash
cargo run -p onto-sdk --bin onto -- --key builder --id ke --roles modeler,reviewer --tier 1 list_tools
```

## Loop wastewater

1. Builder cria tipos/Actions no branch e faz merge.
2. Funnel ingere planta, tanques, sensor, permit.
3. Consumer `propose_setpoint_change` → item de inbox.
4. Supervisor `confirm_action` ou `override_action`.
5. `get_decision_record` replay do dossier.
