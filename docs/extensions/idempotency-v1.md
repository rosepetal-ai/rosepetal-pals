# Extensión A2A `idempotency/v1`

Extensión opcional de A2A (§4.6) con la que un cliente pide que reenviar un mensaje no vuelva a ejecutar el trabajo. Sin ella, un agente PALS se comporta como los SDK de referencia: cada mensaje es nuevo, tenga el `messageId` que tenga.

## Identificación

| Elemento | Valor |
|-|-|
| URI | `https://github.com/rosepetal-ai/rosepetal-pals/extensions/idempotency/v1` |
| Declaración | `capabilities.extensions` de la card, con `required: false` (también en la vista del gateway) |
| Operaciones afectadas | `SendMessage` y `SendStreamingMessage`, en JSON-RPC y en HTTP+JSON |

## Activación

El cliente la activa en cada petición con el parámetro de servicio estándar `A2A-Extensions` (A2A §3.2.6), que en los bindings HTTP es la cabecera del mismo nombre: una lista de URI separadas por comas que incluya la de la extensión. El gateway local la reenvía sin cambios.

La respuesta a una petición que la activa lleva a su vez la cabecera `A2A-Extensions` con la URI de la extensión, como A2A recomienda para las extensiones activadas. Esa cabecera solo dice que el cliente pidió la extensión en una petición que el servidor A2A de este agente atendió. No prueba, petición a petición, que se aplicara la deduplicación: el agente la devuelve en la respuesta de cualquier operación que la pida, también en `GetTask`, `ListTasks`, `CancelTask` y `SubscribeToTask`, donde la extensión no cambia nada, y en errores del servidor A2A. No la llevan las respuestas que la capa de autenticación o de versión da antes de llegar a él (`401`, `403`, la versión no soportada en HTTP+JSON). El gateway devuelve la cabecera sin cambios.

Que reenviar un mensaje sea seguro depende de otras dos cosas: que la card del agente declare la extensión, y que el cliente la active en **todos** los intentos con el mismo `messageId`, el primero incluido. Un primer intento sin la extensión no deja nada que un reintento con ella pueda encontrar, y lo ejecutaría otra vez.

El cliente de PALS (`pals ask`, `pals task resume` y las herramientas MCP) la activa en todos sus intentos: sus reintentos dependen de ella.

## Semántica

La clave es el par `(par que pide, messageId)`; el contenido es el BLAKE3 de la forma JCS de los parámetros de la petición, igual para las dos operaciones y los dos bindings.

- **Primera petición con una clave:** se ejecuta. La clave queda unida a su tarea en cuanto la ejecución empieza, o a su mensaje si el agente responde con un `Message` sin tarea.
- **Repetición con el mismo contenido:** no se ejecuta otra vez. `SendMessage` devuelve la tarea tal como está ahora (o el mensaje de la primera vez); `SendStreamingMessage` sigue la ejecución en curso con `SubscribeToTask`, o emite un único evento con la tarea si ya terminó o no tiene ejecución que seguir.
- **Repetición mientras la primera aún no tiene resultado:** espera hasta 5 s a que lo tenga; después responde `InternalError`.
- **Mismo `messageId` con otro contenido:** `InvalidParamsError` (`-32602` en JSON-RPC, `400` en HTTP+JSON) con el mensaje `messageId reused with different content`. Cualquier diferencia cuenta, también en `configuration` o `metadata`.
- **Reinicios:** la unión sobrevive al reinicio del daemon; una tarea que el reinicio interrumpió queda `TASK_STATE_FAILED` y la repetición la recibe así, sin ejecutarla de nuevo. Una clave que el reinicio dejó sin ejecución se libera al arrancar.
- Dos pares distintos pueden usar el mismo `messageId` sin interferir.

## Retención

Las claves se conservan 7 días; después se borran (al arrancar y cada hora), y un mensaje con esa clave vuelve a ser nuevo.

## Sin la extensión

El agente no consulta ni guarda nada por `messageId`: dos peticiones con el mismo `messageId`, con igual o distinto contenido, son dos mensajes y pueden ser dos ejecuciones, como con el SDK de referencia. La propiedad de las tareas no cambia: cada tarea pertenece al par que la creó.
