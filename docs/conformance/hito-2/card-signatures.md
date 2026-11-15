# Firmas de la Agent Card: verificación independiente

Evidencia de que la Agent Card que firma PALS la verifican, tal cual, los SDK oficiales de A2A de Python, Go y JS, y registro de lo que cada herramienta acepta y de cómo canoniza la card antes de comprobar la firma. Decide H2-D2 (plan del hito 2): **se mantiene `EdDSA` con la propia clave Ed25519 del agente** (rama A de la tarea 1).

**Resultado en una línea:** el vector congelado [`card-jws-v1.json`](../../../crates/pals-core/tests/vectors/card-jws-v1.json), firmado por `pals_core::card_jws::sign_card`, verifica con `a2a-sdk` 1.3.0 (`3 checks passed`), con `a2a-go` v2.7.0 (`verified`) y con `@a2a-js/sdk` 1.3.0 (`verified`). Los tres rechazan una copia con `skills[0].description` cambiado y la card cuando se espera otro agente.

## Ejecución

| Dato | Valor |
|-|-|
| Fecha | 2026-11-14 |
| Firmante | `pals_core::card_jws` en el commit que añade este documento (sobre `be27c49`), Rust 1.99.1 |
| Vector | [`crates/pals-core/tests/vectors/card-jws-v1.json`](../../../crates/pals-core/tests/vectors/card-jws-v1.json): clave de 32 bytes `0x01`, `kid` `8a88e3dd…6f5c`, hash `blake3:856acc06…d4fe` |
| Python | `a2a-sdk==1.3.0` con `PyJWT==2.15.1` (su extra `signing`) y `cryptography==50.0.2`, Python 3.12.3, todo fijado en [`tests/interop/requirements.txt`](../../../tests/interop/requirements.txt) |
| Go | `github.com/a2aproject/a2a-go/v2 v2.7.0` (`a2acrypto`), fijado en [`go.mod`](../../../tests/interop/card_signatures/go/go.mod) y `go.sum`, Go 1.27.3 |
| JS | `@a2a-js/sdk` 1.3.0 con `jose` 6.2.17, fijados en [`package-lock.json`](../../../tests/interop/card_signatures/js/package-lock.json), Node 26.13.0 |

**Reproducir** (desde la raíz del repo, con acceso a PyPI, al proxy de Go y a npm):

```bash
python3 -m venv target/interop-venv \
  && target/interop-venv/bin/pip install -r tests/interop/requirements.txt \
  && target/interop-venv/bin/python tests/interop/card_signatures.py \
       --vector crates/pals-core/tests/vectors/card-jws-v1.json
go -C tests/interop/card_signatures/go run . "$PWD/crates/pals-core/tests/vectors/card-jws-v1.json"
(cd tests/interop/card_signatures/js && npm ci) \
  && node tests/interop/card_signatures/js/verify.mjs crates/pals-core/tests/vectors/card-jws-v1.json
```

El módulo de Go es su propio directorio (no hay `go.mod` en la raíz), por eso `go -C` y la ruta absoluta del vector. `card_signatures.py` acepta también `--gateway URL --peer ID`: lee la card firmada de un par en `URL/peers/ID/signed-agent-card.json` y le aplica las mismas tres comprobaciones. Los de Go y JS aceptan también un id de agente y un fichero con esa card (`go -C tests/interop/card_signatures/go run . <id> <ruta absoluta del fichero>`, `node tests/interop/card_signatures/js/verify.mjs <id> <fichero>`), con las mismas tres comprobaciones; así los usa el caso 3 de la [prueba de salida](exit-test.md#caso-3-validar-cards-superado).

## Qué comprueba cada verificador

Los tres usan el verificador del propio SDK, con un resolvedor de claves que solo confía en el agente esperado: devuelve la clave OKP/Ed25519 cuyo `x` es el `kid` (el id del agente en hex) y solo si el `kid` es ese id.

| Herramienta | Verificador del SDK | Comprobaciones | Salida |
|-|-|-|-|
| [`card_signatures.py`](../../../tests/interop/card_signatures.py) | `a2a.utils.signing.create_signature_verifier(key_provider, ["EdDSA"])` sobre `a2a.client.card_resolver.parse_agent_card(card)` | 1. el vector verifica; 2. con `skills[0].description` cambiado, `InvalidSignaturesError`; 3. esperando el agente de la clave `0x02`, `InvalidSignaturesError` | `3 checks passed` |
| [`go/main.go`](../../../tests/interop/card_signatures/go/main.go) | `agentcard.DefaultCardParser`, y `a2acrypto.NewVerifier` con `KeyResolverFunc` sobre los bytes de la card para cada firma, como hace el resolvedor de cards de Go | las mismas tres | `verified` |
| [`js/verify.mjs`](../../../tests/interop/card_signatures/js/verify.mjs) | `verifyAgentCardSignature(retrievePublicKey)` | las mismas tres | `verified` |

Salida de la ejecución registrada:

```
$ target/interop-venv/bin/python tests/interop/card_signatures.py --vector crates/pals-core/tests/vectors/card-jws-v1.json
check 1 passed: the card verifies with kid 8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c
check 2 passed: a changed skills[0].description is InvalidSignaturesError
check 3 passed: with 8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394 expected instead, InvalidSignaturesError
3 checks passed
$ go -C tests/interop/card_signatures/go run . "$PWD/crates/pals-core/tests/vectors/card-jws-v1.json"
verified
$ node tests/interop/card_signatures/js/verify.mjs crates/pals-core/tests/vectors/card-jws-v1.json
verified
```

## Qué acepta y cómo canoniza cada herramienta

Además del vector, se probaron a mano, con los mismos tres scripts y la misma clave `0x01`, cards que no forman parte del repo:

- una card **no normalizada**, con una extensión `"required": false` y un requisito de seguridad con lista de *scopes* vacía (la `foreign_card` de [`crates/pals-core/tests/card_jws.rs`](../../../crates/pals-core/tests/card_jws.rs)), firmada sobre cada una de las tres formas canónicas de `pals_core::card_jws::CanonicalForm`; en ella las tres formas son bytes distintos;
- la card del vector firmada con `alg: "Ed25519"` (el nombre de RFC 9864) en lugar de `EdDSA`;
- la card del vector con una firma Ed25519 válida pero etiquetada `alg: "ES256"`.

| Firma | `a2a-sdk` 1.3.0 | `a2a-go` v2.7.0 | `@a2a-js/sdk` 1.3.0 |
|-|-|-|-|
| Vector (normalizado: las tres formas son los mismos bytes) | verifica | verifica | verifica |
| Card no normalizada, forma `Raw` | `InvalidSignaturesError` | verifica | no verifica (`JWSSignatureVerificationFailed`) |
| Card no normalizada, forma `ProtoJson` | `InvalidSignaturesError` | no verifica | no verifica |
| Card no normalizada, forma `ProtoJsonClean` | verifica | no verifica | verifica |
| `alg: "Ed25519"` | `InvalidSignaturesError`, también con `algorithms=["Ed25519"]` | no verifica (`unsupported algorithm: Ed25519`) | verifica |
| Firma Ed25519 etiquetada `alg: "ES256"` | `InvalidSignaturesError` | **verifica** | no verifica (`JOSENotSupported`) |

Por herramienta:

- **Python, `a2a-sdk` 1.3.0** (`a2a/utils/signing.py`). `create_signature_verifier` exige la lista de algoritmos (`algorithms`), que pasa a `jwt.decode`. PyJWT 2.15.1 no registra el nombre `Ed25519` (`jwt.algorithms.get_default_algorithms()` no lo incluye), así que la cabecera de PALS usa `EdDSA`. **Canoniza** así: `parse_agent_card` → `ParseDict(…, ignore_unknown_fields=True)` → `MessageToDict` → quita `signatures` → `_clean_empty` (quita recursivamente `None`, `""`, listas y objetos vacíos) → JCS propio (`a2a/utils/_jcs.py`). Es la forma `ProtoJsonClean`.
- **Go, `a2a-go` v2.7.0** (`a2acrypto/verify.go`, `canonical.go`). No recibe lista de algoritmos: toma `alg` de la cabecera protegida y comprueba la firma según el tipo de la clave que devuelve el resolvedor, sin atar uno a otro (por eso acepta una firma Ed25519 etiquetada `ES256`). Su resolvedor de cards (`a2aclient/agentcard/resolver.go`) solo verifica si la card trae firmas: una card sin firmas pasa. **Canoniza** el JSON tal como llegó (con `UseNumber`), quita `signatures` de primer nivel y aplica su JCS. Es la forma `Raw`.
- **JS, `@a2a-js/sdk` 1.3.0** (`dist/index.js`, `verifyAgentCardSignature` y `canonicalizeAgentCard`). Exige `kid`, `typ` y `alg` en la cabecera protegida y verifica con `jose.flattenedVerify`; `jose` 6.2.17 acepta tanto `EdDSA` como `Ed25519` y comprueba que el algoritmo cuadre con la clave. **Canoniza** con `AgentCard.toJSON(AgentCard.fromJSON(card))`, quita `signatures` y aplica `cleanEmpty`, que quita lo mismo que el `_clean_empty` de Python, antes de su JCS. En la prueba se comporta como la forma `ProtoJsonClean`, igual que Python.
- **`a2a-inspector`**, commit `30c10de` (2026-11-09, ahora en Go): **ignora las firmas**. Solo tiene los tipos generados (`signatures`, `AgentCardSignature` en `ui/src/a2a/*.gen.ts`); no hay en su código ninguna llamada a `a2acrypto` ni otra verificación. Por eso el plan usa los verificadores de los SDK en su lugar.

**Diferencia con el plan.** El plan atribuía a `@a2a-js/sdk` 1.3.0 la forma `ProtoJson` (viaje por protobuf sin quitar vacíos) y suponía que los tres SDK canonizaban distinto. Con las versiones fijadas, JS canoniza como Python (`ProtoJsonClean`): en la tabla, la card no normalizada firmada sobre `ProtoJson` no la verifica ningún SDK. `pals_core::card_jws` sigue aceptando las tres formas al verificar una card ajena, como pide la interfaz de la tarea 1; la forma `ProtoJson` queda para firmantes que no quitan vacíos.

## Consecuencias para PALS

- **Firma.** Cabecera protegida exactamente `{"alg":"EdDSA","kid":"<agent id en hex>","typ":"JOSE"}` (JCS), sin cabecera sin proteger. La entrada JWS empieza siempre por `eyJ`, nunca por un dominio `agentnet.`, así que una firma de card no vale como firma de grant o de evento ni al revés.
- **Cards normalizadas.** Como cada SDK canoniza distinto, PALS solo firma cards cuyas formas `Raw`, `ProtoJson` y `ProtoJsonClean` son los mismos bytes (`card_jws::normalize`): sin `required: false`, sin opcionales vacíos, sin listas de *scopes* vacías y sin campos obligatorios vacíos. Una card con valores por defecto firmada por PALS solo la verificaría Go.
- **Verificación propia.** Una firma vale solo con `alg` `EdDSA`, sin `crit`, con `kid` igual al agente esperado y con una firma Ed25519 que pasa las reglas estrictas de `ed25519-dalek`. Antes se valida la card recibida: 32 KiB, profundidad 16, cadenas de 4 KiB, 8 interfaces, 64 skills, 16 extensiones, 4 firmas y una interfaz A2A 1.0. Una card ajena vale con una firma válida sobre cualquiera de las tres formas.
- **Qué queda cubierto por la firma.** La card que devuelve la verificación (`VerifiedCard::card`) se lee de los bytes que cubre la firma válida (`VerifiedCard::payload`), nunca del JSON recibido, y tiene que conservar una interfaz A2A 1.0. Las cards de PALS verifican en la forma `Raw`, y en ella todo el JSON recibido, salvo `signatures`, está firmado. Con una forma protobuf, en cambio, el JSON recibido (`VerifiedCard::json`) puede llevar contenido que la firma no cubre, que nunca llega a la card:
  - claves desconocidas;
  - nombres en snake_case que solo leen los tipos protobuf;
  - requisitos de seguridad con la forma que solo lee `a2a-lf`.

  El hash (`VerifiedCard::hash`) es el del JSON tal como se recibió: una card que ganó contenido sin firmar por el camino da un hash distinto del de la copia de su firmante.
- **Rama B no necesaria.** El SDK de Python verifica `EdDSA`, así que no hace falta la clave `ES256` aparte ni la declaración `agentnet.card-key/1` de la reserva de H2-D2.
