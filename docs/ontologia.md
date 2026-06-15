# Domain Ontology

The domain ontology for Prisma lives in the `prisma` server repo and is shared by both repos.

**Do not duplicate it here.** Read the source directly:

```
../prisma/docs/ontologia.md          ← entity map, mechanics, axioms, not-yet-implemented
../prisma/docs/concepts/<entity>.md  ← one file per concept (Stream, Source, Note, ZoteroItem, …)
```

On GitHub: `CServinL/prisma/docs/ontologia.md`

## Why it lives in prisma, not here

The ontology is grounded in the server's Pydantic models. When a model changes, the ontology
should be updated in the same commit. Keeping them in the same repo enforces that discipline.

prisma-desktop consumes the ontology but does not define it.
