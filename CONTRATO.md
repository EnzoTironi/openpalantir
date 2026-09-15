# Contrato de integração

Modelo escolhido: **Engine Rust escritor canônico**. Não há segunda ontologia editável. O host Zoen lê e aciona por este contrato. O validador-Rust + host-PG-efetiva **não** é o modelo deste repositório.

## Dono da transação

[`Store`](crates/onto/src/store.rs) é o único seam de persistência. Ele possui a conexão e o lock. [`SqliteStore::commit_decision`](crates/onto/src/store.rs) é a unidade transacional do comando: CAS do read-set e da revisão de schema, reserva da chave de idempotência (ligada ao digest), write-set, consumo exclusivo da inbox (`pending` → `confirmed`), `DecisionRecord` e intenção de efeito. Autorização e guardas são reavaliadas se o CAS falhar. Nenhuma rede dentro da transação.

[`Engine`](crates/onto/src/engine.rs) coordena. Não fala SQL. Recebe o store por [`Engine::from_store`](crates/onto/src/engine.rs). SQLite é o backend de ensino.

## Dono do efeito

O kernel declara a intenção (`effect_intentions.status = declared`) no mesmo commit da decisão. **Allow / committed é estado interno da ontologia, não entrega externa.** O host despacha, tenta, acusa e reconcilia. O kernel não marca delivered.

## Autoridade

Sessão vem do host autenticado. Argumentos do modelo não escolhem `roles` nem `tier`. O adaptador MCP local tem três chaves de processo (`--key builder|consumer|reviewer`). Consumer é T2 leitura/proposta. Reviewer é T3 confirmação, separado. Política em quatro níveis acompanha leitura, write-set, snapshot, metadados e título.

## Tempo

`Engine::open` / `from_store` usam o relógio de parede. `Engine::memory` congela `1_700_000_000` para testes. `valid_from` é observação/efetivo. `tx_from` é o relógio de registro. Evento efetivo mais antigo não substitui a versão corrente.

## Compensação

Action inversa nomeada. Fecha relações ativas. Não é rollback da linha original (Zhang 2026).
