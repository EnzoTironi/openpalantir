# Contrato de integração

Modelo escolhido: **Engine Rust escritor canônico**. Não há segunda ontologia editável. O host Zoen lê e aciona por este contrato. O validador-Rust + host-PG-efetiva **não** é o modelo deste repositório.

## Dono da transação

[`Store`](crates/onto/src/store.rs) é o único seam de persistência. Ele possui a conexão e o lock. [`SqliteStore::commit_decision`](crates/onto/src/store.rs) é a unidade transacional: write-set composto (read-your-writes), consumo exclusivo da inbox (`pending` → `confirmed`), `DecisionRecord`, chave de idempotência e intenção de efeito. Nenhuma rede dentro da transação.

[`Engine`](crates/onto/src/engine.rs) coordena. Não fala SQL. Recebe o store por [`Engine::from_store`](crates/onto/src/engine.rs). SQLite é o backend de ensino.

## Dono do efeito

O kernel declara a intenção (`effect_intentions.status = declared`) no mesmo commit da decisão. **Allow / committed é estado interno da ontologia, não entrega externa.** O host despacha, tenta, acusa e reconcilia. O kernel não marca delivered.

## Autoridade

Sessão vem do host autenticado. Argumentos do modelo não escolhem `roles` nem `tier`. Chaves builder e consumer permanecem separadas. Política em quatro níveis acompanha leitura, write-set, snapshot e título.

## Tempo

`valid_from` é observação/efetivo. `tx_from` é o relógio de registro do Engine. Funnel atrasado não confunde os dois eixos.

## Compensação

Action inversa nomeada. Fecha relações ativas. Não é rollback da linha original (Zhang 2026).
