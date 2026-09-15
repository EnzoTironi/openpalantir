# Operational Ontology OS

Runtime de ontologia operacional em Rust: OMS vivo, object store, OSS, Actions, Functions e Funnel. Sem UI. A interface é MCP (e o SDK/CLI que falam a mesma superfície).

Inspirado em Zhang, *Operational Ontology: From Business Mirror to Decision Runtime* (First Public Edition, v1.0, 2026). Código original. Não é um produto Palantir e não usa nomes de produto na API pública.

## Regras

- A constituição mora no **OMS**. Tipos de domínio nascem em runtime (`open_branch` → create/alter → `submit_proposal` → `review_proposal` → `merge_to_main`).
- Funções são registros OMS (`create_function` no branch). Propriedades `Derived` nomeiam a função; o motor consulta o registro. Não há `if` no nome da propriedade.
- Só o meta-modelo é compilado: `ObjectType`, `PropertyType`, `ValueType`, `LinkType`, `InterfaceType`, `ActionType`, `FunctionType`, `Policy`, `OntologyBranch`, `OntologyProposal`. Depois de `Engine::memory()`, cada nome é um registro `ObjectType` consultável (`search_objects` com tipo `ObjectType`).
- **Interfaces são contratos (Zhang 2026, Ch. 4–5):** `Reviewable` num ActionType em modo Propose cria objeto de inbox; `Evidenced` num ObjectType faz o submit que o lê falhar Complete (Review) se a evidência obrigatória falta ou está velha. Attach não mergeado não vale no `main`.
- **Chave consumer:** consultar o mundo e executar Actions pré-definidas. Não edita schema. Instâncias são deny-by-default em quatro níveis (Zhang 2026, Ch. 10): plataforma (consumer vs builder), tipo, instância, propriedade. Sem grant é `Deny`. Propriedade `Deny` some na leitura (`get_object` e object sets), não só `rationale`.
- **Chave builder:** edita schema num branch. Não lê nem escreve instâncias de produção.
- Toda escrita de negócio passa por Action no write path de 7 passos (`WritePathStep`): submit → param+permission → submission criteria → staged edits (all-or-discard) → commit atômico → selar `DecisionRecord` → declarar side effects com chave de idempotência. Funnel não sobrescreve propriedade `ActionWritten`. `create_link` não é API pública; links nascem por Action (`assert_link`) ou Funnel.
- Compensação de um `Allow` submete a Action nomeada em `ActionTypeSpec.compensation` pelo mesmo write path (`Engine::compensate_action`). Sela um `DecisionRecord` novo. O original permanece. Versões de objeto só acrescentam (Zhang 2026, Ch. 9: compensate forward). Sem nome de compensação é `OntoError::NoCompensation`, não sucesso silencioso. Retry com a mesma chave de idempotência não aplica de novo.
- Instâncias são versionadas (Zhang 2026, Ch. 5): cada escrita de propriedades **acrescenta** um `VersionSpan` (valid time + transaction time). `get_object(id)` lê a versão aberta; `get_object(id, as_of)` reconstrói o objeto no tempo válido. Ponta aberta é `None`. Não há overwrite in-place na tabela `objects`.
- `DecisionRecord.data_snapshot` pina os objetos lidos nos guards, mais `rule_version`, `function_version` (digest das funções invocadas) e `engine_version`.
- **Níveis de agente (Zhang 2026, Ch. 11):** `AgentTier` T1–T4. T1 observa (search/get/traverse/aggregate/missing evidence); T2 propõe (`describe_action`, `submit_action`); T3 confirma (`list_inbox`, `confirm_action`, `override_action`; confirmer ≠ proposer); T4 auto só dentro de um bound `{action_type × object_set × risk_band}` (bound vazio = sem auto). `list_tools` e `dispatch` escondem e recusam syscalls acima do nível. `Actor.tier` não é `u8` livre.
- Inbox é objeto, não tela. Confirmar/vetar é Action.
- **OSS:** `ObjectSet` é spec tipada (tipo + filtro + nome opcional), não uma lista de ids. `search_objects` avalia o conjunto depois do filtro de permissão. Conjuntos nomeados nascem num branch de builder (`create_object_set`) e só aparecem no `main` depois do merge. Conjunto vazio é `Ok([])`. `aggregate` conta o conjunto filtrado.

## Crates

- `crates/onto` — motor
- `crates/onto-sdk` — cliente + CLI `onto`
- `crates/onto-mcp` — MCP stdio e HTTP em `:43177/mcp`
- `crates/onto-bootstrap` — casos wastewater, ensino superior e healthcare via as mesmas APIs de builder

## Lint

Clippy pedantic está em deny no workspace. `SQLite` é o backend de ensino; [`Store`] em `crates/onto` é dono do lock da conexão.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D clippy::pedantic -D warnings
cargo test --workspace -- --test-threads=1
```

## Utilidade (comprador)

Um comando. Cada reivindicação aponta para um teste `does_*`. Não é bench de latência. Texto em [UTILIDADE.md](UTILIDADE.md).

```bash
cargo run -p onto-bootstrap --bin usefulness
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

Caso de ensino Zhang 2026 (cite; não copie o texto). Instalação em runtime via `onto_bootstrap::install`. Sem YAML.

1. Builder cria tipos/Actions no branch e faz merge.
2. Funnel ingere planta, tanques, sensor, permit.
3. Consumer `propose_setpoint_change` → item de inbox.
4. Supervisor `confirm_action` ou `override_action` (confirmer ≠ executor).
5. `get_decision_record` replay do dossier.
6. Supervisor `compensate_action` no Allow: Action inversa (`revert_setpoint_change`), não rollback.

Testes nomeados (`crates/onto-bootstrap/tests/wastewater_case.rs`):

- sensor stale ou sem `last_reading_at` → `Review` + `request_sensor_calibration`
- `target_do` acima do permit → `Deny`
- ator sem papel → `Deny`
- override sela `DecisionRecord` replayável; confirmer ≠ proposer
- Funnel não sobrescreve `AerationTank.target_do` (`ActionWritten`)
- schema em branch não mesclado não altera instâncias de produção

## Loop ensino superior (Zhang 2026)

Turma cheia entra em fila (`Waitlist`). `propose_enroll` não faz Auto: vai para a inbox de Review. O supervisor confirma e `approve_enroll` atribui o `Seat`, ou `Deny` se `slots` já é 0. Segunda confirmação no mesmo assento não faz double-book. Compensação é `release_seat` (inversa para frente). Sem tipos de saúde.

```bash
# mesmo comando; testes em crates/onto-bootstrap/tests/highered.rs
cargo test --workspace -- --test-threads=1
```

## Loop healthcare

Caso de ensino Zhang 2026 (cite; não copie o texto). Instalação em runtime via `onto_bootstrap::install_healthcare`. Sem YAML. Instrucional: o motor **não calcula quantidade prescrita**.

1. Builder cria Patient, Order, Observation, DecisionRecord no branch e faz merge.
2. Funnel ingere paciente, pedido e observações (completa, evidência ausente, flag de contraindicação).
3. Consumer `propose_order` com evidência completa → item de inbox.
4. Observação sem `last_reading_at` (ou stale) → `Review` + `request_observation`. Não é Allow.
5. Flag de contraindicação (`coded_clearance = 0`) → `Deny` no mesmo molde do permit.
6. Supervisor `confirm_action` grava `DecisionRecord`.
