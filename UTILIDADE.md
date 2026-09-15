# Utilidade — ontologia operacional

Runtime em Rust para um OMS vivo. Agente primeiro, zero UI. A superfície é MCP (e o SDK/CLI que falam a mesma API). Inspirado em Zhang, *Operational Ontology: From Business Mirror to Decision Runtime* (First Public Edition, v1.0, 2026). Código original. Não é um produto Palantir e não usa nomes de produto na API pública.

Isto não mede latência. Cada linha do placar é uma reivindicação de comprador ligada a um teste `does_*`. Se o mecanismo some, a linha vira FAIL.

## Um comando

```bash
cargo run -p onto-bootstrap --bin usefulness
```

Imprime `reivindicação → teste → PASS/FAIL` e sai `0` só se todas as linhas passam.

## O que o placar cobre

- Constituição no OMS, não em YAML. Tipos de domínio nascem num branch e só valem no `main` depois do merge.
- Chaves separadas: builder edita schema; consumer consulta e executa Actions.
- Toda escrita de negócio passa pelo write path de 7 passos. Falha de guarda descarta o stage.
- Inbox é objeto (`Reviewable`). Evidência ausente ou velha é `Review`, não `Allow`.
- Funnel não sobrescreve propriedade `ActionWritten`. `create_link` não é API pública.
- Instâncias versionadas (`as_of`). Compensar um `Allow` é Action inversa, não rollback.
- Object set é spec, não lista de ids. Sem grant é `Deny`. Propriedade `Deny` some na leitura.
- Agentes T1–T4. T4 auto só dentro de um bound declarado.
- Casos de ensino: efluentes, ensino superior (sem double-book), healthcare instrucional — o motor não calcula quantidade prescrita.

Cite Zhang 2026; não copie o texto do livro.
