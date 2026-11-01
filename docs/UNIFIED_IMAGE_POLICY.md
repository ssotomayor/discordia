# Política común de imágenes y visor progresivo

Rama: `perf/phase-8-image-memory`. Continuación autorizada sobre `d9a688c`;
el remoto de Fase 8 estaba en `58ff9d3`. Base del PR: `perf/ram-optimization`
(`79ffc91`); se conserva el commit heredado `b753f11` con el informe ThinLTO.
No se repiten benchmarks de Fases 1–7. No se modifica el globo, CI, releases
ni instaladores.

## Decisión y resultados

Conservar la política compartida y el visor progresivo. El ahorro demostrado
es del recorrido de comunidades con JPEG grandes; no se demuestra un ahorro
adicional general en DMs. No sumar este resultado al ahorro de buffers del
tramo inicial: usan escenarios distintos.

Medianas de cinco procesos Windows/WebView2, ocho imágenes y DPR 1:

| Recurso / recorrido | Private Commit total anterior → común, MiB | Carga preparada anterior → común, ms |
|---|---:|---:|
| JPEG 1920×1280, comunidad | 193,47 → 172,13 | 41,42 → 27,96 |
| JPEG 1920×1280, DM | 172,51 → 172,13 | 19,11 → 27,96 |
| JPEG 4000×3000, comunidad | 256,25 → 174,27 | 172,58 → 28,25 |
| JPEG 4000×3000, DM | 174,20 → 174,27 | 27,01 → 28,25 |
| PNG RGBA 2560×1440, comunidad | 169,09 → 167,03 | 27,57 → 19,12 |
| PNG RGBA 2560×1440, DM | 169,00 → 167,03 | 19,70 → 19,12 |

JPEG grande de comunidad: **81,98 MiB menos de Private Commit total (32,0%)**;
Rust 82,00 → 8,21 MiB, Renderer 23,45 → 26,38 MiB, GPU Private 91,46 →
80,54 MiB y Working Set sumado 432,83 → 344,37 MiB. CPU de carga del
árbol: 0,5625 → 0,046875 segundos de núcleo. No equivale a VRAM ni a RAM
del cliente completo. El ahorro principal viene de los recursos retenidos
en Rust; CSS por sí solo no demuestra que Chromium decodifique todo a RGBA.

La calidad JPEG pasa de 75 en el DM anterior a 92. En JPEG medio, el DM
preparado tarda 8,85 ms más por lote de ocho y Rust aumenta 0,41 MiB;
en JPEG grande Rust aumenta 1,71 MiB. Cambian tanto el recurso como el
componente de presentación: no se atribuye todo el coste a un único factor.
Se acepta el intercambio por una política compartida conservadora de calidad,
orientación y animación; no se lo presenta como una mejora de rendimiento DM.
Las diferencias pequeñas de CPU/consumo tienen rangos solapados. El PNG DM
anterior y el nuevo son idénticos: sus diferencias se tratan como variabilidad.

Tablas completas, mínimos, máximos y desviación estándar:
[mediciones](UNIFIED_IMAGE_MEASUREMENTS.md),
[estadísticas y preparación](UNIFIED_IMAGE_STATISTICS.json),
[muestras crudas y hashes de binarios/recursos](UNIFIED_IMAGE_RESULTS.json).

## Flujo y archivos

| Área | Implementación |
|---|---|
| Motor compartido | `image-policy/`; máximo 640×480, sin ampliar imágenes pequeñas. JPEG calidad 92; PNG conserva formato y alfa. Límites de entrada 10 MiB, 16 millones de píxeles, dimensiones 8192 y asignación del decoder 128 MiB. Derivada hasta 2 MiB |
| Color y animación | Aplica orientación EXIF a los píxeles; copia ICC RGB/GRAY acotado. GIF, APNG, WebP y AVIF conservan original. PNG con transferencias/primarias no estándar o metadatos HDR conserva original porque el encoder no los puede reproducir. No se afirma pausa de WebP/APNG |
| Comunidades | `protocol/src/lib.rs`, `server/src/media.rs`, `server/src/gateway/connection.rs`, `client/src/net.rs`, `features/chat.rs`. Ready anuncia capacidad opcional, ausente=false. El cliente solicita miniaturas al montar imágenes del chat; no requiere tocarlas. Servidores antiguos conservan el recorrido original |
| Caché servidor | Sidecars versionados dentro de MediaStore, contados en la cuota existente. Subpresupuesto min(128 MiB, cuota/8), expulsión de las más antiguas antes de rechazar una subida de originales. La referencia al original protege su derivada vigente en el barrido |
| Saturación | Dos trabajos activos de servidor, 16 admitidos globalmente y hasta ocho por conexión. Hasta 64 nombres canónicos por solicitud; respuestas acotadas para permitir un original máximo. Sin cupo, respuesta vacía y reintento existente del cliente tras ocho segundos. Trabajos bloqueantes fuera del socket; los ya iniciados pueden terminar al cerrar la conexión. No se garantiza equidad ni tiempo máximo de espera bajo carga sostenida |
| Caché cliente | `media_cache.rs`; claves preview/original distintas dentro del mismo LRU de 128 MiB/2048 entradas desktop (64 MiB Android). Los originales abiertos pueden seguir en ese LRU para evitar otra descarga. ImageAssets conserva liberación de buffers con la última vista |
| DMs | `nostr/dm_image.rs`, `features/dm_images.rs`, `nostr/service.rs`, `state.rs`. Descarga/descifra el original para generar la miniatura local; caché cifrada `.preview-v2.thumb`. No cambia AES, NIP-17 ni transporte Blossom. Se ignoran miniaturas antiguas sin borrar ciphertext ni mensajes |
| Presentación | `features/image_attachment.rs`, `image_assets.rs`; mismo CSS máximo 320×240. Por encima de DPR 2 se usa original; un visor DM de alta densidad conserva original mientras siga activo. Los ahorros a 1× no se generalizan a pantallas de mayor densidad |
| Visor progresivo | `features/image_viewer.rs`, `assets/image-viewer.js`, `image-viewer.css`. Abre con la miniatura y estado de carga; carga normal del navegador, sin forzar decode completo. Sustituye la fuente conservando instancia, encuadre, zoom y desplazamiento. Fallos conservan miniatura y ofrecen reintento; tiempo de espera 30 s, intentos identificados y cierre protegido contra respuestas tardías. Cerrar puede dejar una descarga compartida terminando en la caché acotada |
| Aperturas existentes | `features/bookmarks.rs` y emojis mantienen apertura de imágenes ya disponibles, adaptada al estado tipado del visor |

## Coste frío y límites de las mediciones

MediaStore Release, cinco procesos independientes, JPEG 4000×3000:
generación y escritura inicial **72,02 ms** [71,17–72,34], SD 0,57;
lectura cacheada **3,41 ms** [3,33–4,31], SD 0,43. Se excluyen subida,
red, cola y socket. Los límites de cola se validan por prueba funcional;
no se midió rendimiento del servidor bajo una ráfaga real de usuarios distintos.
La primera solicitud de un lote debe esperar su generación; los 28,25 ms
del componente no son el tiempo total de una recepción fría ni de ampliar.

Las 45 ejecuciones comparan componentes reales en Windows 10.0.26300,
WebView2 156.0.4314.58, Ryzen 7 9700X, 32 GB; perfiles nuevos, ventana
visible y enfocada, 1440×900, DPR 1. Se intercalan variantes y se invierte
el orden en rondas pares, sin compilaciones paralelas. La app existente
PID 10772 se preserva. El profiler sigue únicamente cada árbol de prueba.

El visor progresivo se agregó después del A/B de miniaturas. Sus cinco
sesiones nativas prueban la secuencia y liberación de buffers; no constituyen
un A/B de consumo/latencia del visor completo ni de red real. Su transporte
es controlado; gateway y cifrado se prueban por separado.

## Errores detectados y controles

| Hallazgo | Resolución / evidencia |
|---|---|
| GIF grande convertido a un único fotograma en el DM previo | Passthrough de animaciones; GIF real avanza en WebView2 y conserva bytes; APNG de dos frames en tests |
| EXIF perdido y ICC no copiado | Rotación física y copia acotada; test de píxeles, perfil y vista nativa |
| Metadatos PNG de color no reproducibles por el encoder | Passthrough conservador; tests de gamma estándar/personalizada, HDR y WebP |
| Límite de respuesta 8 MiB insuficiente para originales codificados | Presupuesto admite original máximo; gateway probado con GIF válido de 7 MiB |
| Cola limitada sólo por conexión; nombres inválidos podían quedar retenidos | Límite global, filtro temprano y prueba de saturación/cancelación de cupos |
| Densidad emulada sin eventos nativos | Listener de media query y respaldo resize, sin polling. QA 3× usa redimensionado sintético explícito; cambio físico entre monitores pendiente |
| Cambio de fuente remontaba el visor y reiniciaba zoom | Clave de sesión estable; cinco pruebas nativas conservan DOM, puente, tamaño y transformaciones |
| Resultado tardío/reintento podía afectar otra apertura | Identidad de sesión e intento; pruebas de cierre, error de imagen y resultados obsoletos |
| Caché negativa impedía reintentar originales ausentes | Se invalida sólo la entrada vacía y su marcador de solicitud al reintentar |
| Documentación confundía el límite de artwork con adjuntos | Mapa/test corregidos: comunidad 10.000.000 bytes; DM 10 MiB; artwork conserva 2 MiB |

## Validación y reproducción

- Cliente: chat_image, ImageAssets, MediaCache, DM cifrado, net, liberación por canal, E2EE y estado del visor. Subida externa Blossom queda ignorada; no se presenta como prueba superada.
- Protocol: 78 tests; image-policy: seis tests. Servidor: acceso a medios, metadatos, preview/gateway, cuota, reinicio/barrido y admisión global/cancelación.
- [12 controles nativos de política](UNIFIED_IMAGE_CHECKS.json) y [cinco sesiones del visor](PROGRESSIVE_IMAGE_CHECKS.json); automatizados, no manuales.
- Clippy de crates modificados con `-D warnings`, formato y comprobación de whitespace. Regresión JS de GIF, adjuntos, pantalla compartida y worker E2EE.
- Suite multimedia WebView2 completa: no ejecutada. Cargo no pudo reemplazar `target/debug/Discordia.exe` (PID 10772, otra sesión), incluso seleccionando sólo el test. Se preservó ese proceso. CI o una sesión sin ese bloqueo deberá ejecutarla; no se registra como test fallido de funcionalidad. La suite usa dobles de medios y tampoco sustituye una llamada real entre usuarios.

Reusar `scripts/measure-image-memory.cjs` y `scripts/measure-ui-memory.ps1`:
`measure-unified-images.cjs` verifica corpus y ejecuta las nueve variantes;
`summarize-unified-images.cjs` genera estadísticas. `check-unified-images.cjs`
verifica política; `check-progressive-images.cjs` prueba el visor. El probe usa
`--features unified-policy` para A/B y `--features progressive-viewer` para
el visor con transporte controlado. Baseline: ejecutable de `d9a688c`; se
conservan sus recursos bajo `target/dm-server-images`. Preparar corpus con
`prepare-unified target/unified-images/fixtures <1..5>`. Los hashes históricos
de binarios identifican exactamente qué se midió; los hashes de fuentes finales
incluyen los ajustes posteriores del visor y la admisión.

## Compatibilidad pendiente antes de fusionar

Pruebas manuales con dos cuentas reales: envío/recepción en comunidad y DM,
subida lenta/fallida, calidad visual percibida, perfiles/avatares/emoji/fondos,
cambio físico de densidad y uso simultáneo con voz, cámara y pantalla compartida.
Los originales y el cifrado se verificaron automáticamente; no se afirma una
prueba manual de llamadas o de transferencia externa. No se midió VRAM por
falta de API fiable atribuible, ni rendimiento Android/macOS/Linux.

Este PR conserva generación en servidor para comunidades y en receptor para
DMs. Generación en emisor y publicación de miniatura antes de subir el original
requieren otro diseño de protocolo; no están implementadas aquí.
