# Fase 8 — imágenes y memoria gráfica

## Referencia y alcance

Rama inicial `perf/ram-optimization`, árbol limpio al iniciar en `b753f11`.
GitHub confirmó `origin/perf/ram-optimization` en `79ffc91`: el commit de
Fase 7 seguía siendo local. No se repitieron sus benchmarks. No se audita
ni modifica el globo. No se modifican CI, instaladores, releases o protocolo.

Windows 10.0.26300, Ryzen 7 9700X (16 procesadores lógicos), 32 GB de RAM;
WebView2/Edge 156.0.4314.58, Rust 1.99.0, Node 24.22.1 e image 0.25.10.
Se usaron PowerShell, Git/Cargo, el profiler existente y CDP local.

**Decisión final:** conservar únicamente la vida útil de buffers de
ImageAssets. Ahorro medido del host Rust al desmontar el corpus: **48,63 MiB**.
No es una reducción medida del cliente completo ni de VRAM. Se descartan
las derivadas JPEG locales por latencia y complejidad sin beneficio de CPU
total demostrado. No queda transformación nueva de imágenes en producción.

## Auditoría del recorrido

| Recurso | Recepción, renderizado y liberación existentes | Archivos revisados |
|---|---|---|
| Adjuntos de servidor | Sentinel `media:<hash>.<ext>`; `FetchEmoji` entrega el original como data URL, con deduplicación por dirección y lectura de disco fuera del hilo UI. `ChatImage` usa URL local de `ImageAssets`; muestra el original con CSS máximo 320×240. El visor solicita el mismo original. No hay miniatura de red ni resolución adaptada en Rust en este recorrido. El chat ya virtualiza páginas de 50 mensajes: una página inactiva conserva sólo su altura, sin montar imágenes. No corresponde agregar otra virtualización | `server/src/gateway/connection.rs` (`FetchEmoji`), `client/src/net.rs` (`EmojiBlobs`, `request_missing_images`), `state.rs` (`media_src_shared`), `features/chat.rs` (`ChatImage`, `MessagePage`), `features/image_viewer.rs` |
| Adjuntos DM | AES-GCM antes de Blossom; aceptación de conversación antes de descargar. Caché de ciphertext y miniatura cifrada. `receive` reutiliza miniatura 640×480; una caché fría requiere descargar el original para generarla localmente. `load_original` descifra el original sólo al abrirlo; reutiliza ciphertext en disco. Semáforos de dos descargas; generación fuera de UI. Cambio de DM suelta `data` de otros canales y conserva historial | `nostr/dm_image.rs` (`receive`, `load_original`, `cache_store`), `features/dm_images.rs` (`EncryptedImage`), `chat_image.rs` (`thumbnail_data_url`), `state.rs` (`evict_inactive_dm_images`) |
| Avatares/banners/iconos | Referencias por hash o imágenes personales compartidas con `Arc`. `use_profile_image` resuelve por dirección; `ImageAssets` compara hash y contenido para compartir URL. Avatares se escalan con CSS, sin variante por tamaño. Cambiar contenido invalida la URL por contenido. Crop limita salida a 2 MiB; originales de editor de avatar 8 MB, banner 10 MB | `features/profiles.rs`, `features/guilds.rs`, `profile.rs`, `image_edit.rs`, `image_assets.rs` |
| Emojis/GIF | Emojis de servidor resueltos desde MediaCache; `EmojiText` usa data URL/URL remota directamente. NIP-30 puede usar URL externa o datos inline limitados. `gif-playback.js` ya observa intersección, foco y visibilidad; muestra poster cuando no reproduce. Su poster usa dimensiones naturales y PNG; WebP animado no entra en esa detección GIF | `features/chat.rs` (`EmojiText`), `emoji.rs`, `assets/gif-playback.js`, `app.rs` (`AppHead`) |
| Fondos | Selección hasta 4 MB, sin límite de píxeles explícito en ese selector. Se guarda data URL en settings y se usa como fondo CSS de la capa de aplicación y como `<img>` en el selector. No usa ImageAssets. Reemplazar fondo actualiza la capa; la liberación de bitmap/textura depende del motor | `features/appearance.rs`, `app.rs` (`app-bg-layer`), `settings.rs` |

| Caché | Límites/política existentes | Hallazgo |
|---|---|---|
| MediaCache | Desktop 128 MiB de data URLs / 2048 entradas; Android 64 MiB. LRU por acceso. Pendientes desktop hasta 128 MiB y 2048; Android 32 MiB. Worker de disco con cola de ocho jobs. Misses agrupados/deduplicados | Comparte bytes entre estado, vistas y escritura. Preserva imágenes de canales recientes hasta expulsión; no debe confundirse su presupuesto con bitmaps del navegador |
| ImageAssets | Registro por ventana, hash con igualdad exacta, owner débil del data URL, barrido de owners muertos cada 64 registros. Respuesta `private, max-age=300`. Campo `decoded` comparte bytes de PNG/JPEG/GIF/WebP, no píxeles RGBA | Si MediaCache conserva el data URL, el owner sigue vivo después del cierre de la vista. La copia binaria servida queda retenida aunque no haya ninguna vista, hasta perder el owner y ejecutar el barrido |
| DM ciphertext | Desktop 256 MiB, Android 128 MiB, expiración de siete días y hasta 256 archivos, con espacio reservado para la escritura siguiente | Se conserva cifrado. Las imágenes Ready sin vistas se limitan y se liberan al cambiar de DM; las activas quedan protegidas. Los datos de imagen no son la historia del mensaje |
| WebView2 | Caché HTTP/decodificación/recursos gráficos del motor; no es MediaCache ni ImageAssets | No se fuerza vaciado global ni trimming de Working Set. DOM desmontado no garantiza retorno inmediato de páginas al sistema operativo |

El controlador GIF tiene un `Map` por elemento con fuente y poster PNG, sin
presupuesto explícito de bytes; al desconectar el elemento quita observación,
listeners y entrada. Libera el backing del canvas después de crear el poster.
Una fuente remota sin CORS puede impedir crear poster y, por tanto, pausarla
con ese mecanismo. No controla fondos CSS, APNG ni WebP animado. No existe
otra caché propia de fotogramas en Rust: sus copias son datos comprimidos;
la caché de fotogramas/píxeles del motor no tiene contador fiable en este banco.

La política DM Ready intenta conservar hasta ocho imágenes en memoria,
descontando las vistas activas; no expulsa una imagen que tenga viewers.
Al desmontar se retira el viewer y al cambiar de DM se sueltan los datos
inactivos. Este límite es distinto de los 256 archivos cifrados en disco.

## Priorización

| Prioridad | Problema verificable | Evaluación |
|---|---|---|
| Alta | Copias binarias de ImageAssets sobreviven a la última vista mientras MediaCache conserva el original | Medir bytes retenidos y Private Commit; preservar URL estable y deduplicación para evitar decodificaciones por cambios de URL |
| Alta | Adjuntos estáticos de servidor usan originales grandes en miniaturas CSS | Comparar una derivada local con el original disponible al abrir. Medir primero: dimensiones naturales no prueban cuánto decodifica o retiene realmente Chromium |
| Media | Avatares de resolución mayor que su celda | Escenario de control; no cambiar sin A/B y control de calidad |
| Media | Posters de GIF con resolución natural, y fondos grandes | Escenarios de control. Mantener bytes, fotogramas y animaciones; no cambiar políticas del motor a partir de hipótesis |

## Banco e instrumentación

`tools/image-memory-probe` incluye el archivo de producción `image_assets.rs`
y ejecuta sus hooks y handler reales en Dioxus desktop/WebView2. Usa únicamente
dependencias ya presentes en el proyecto; no arrastra la aplicación, SFU ni
WebRTC. Los datos son deterministas y generados localmente, sin identidades,
red ni archivos del usuario. El corpus residente no supera 128 MiB de data
URLs: simula un MediaCache poblado, sin sustituir ni modificar su LRU.

`scripts/measure-image-memory.cjs` reutiliza `measure-ui-memory.ps1` con PID
explícito y descendencia. Cada variante usa cinco procesos independientes,
datos WebView nuevos y el mismo corpus; conserva hashes del corpus/binario.
Escenarios: múltiples adjuntos, alta resolución, avatares/emojis, GIF,
seis cambios de canal, seis cambios de DM, seis aperturas/cierres de original,
minimización/restauración, reemplazo de fondo y desmontaje final.

Las series concluyentes `paint-*` esperan `complete`, dimensiones válidas y
dos frames, sin invocar `HTMLImageElement.decode()`. Registran la latencia
de carga DOM y dos frames, no un instante exacto de presentación física.
Esperan un segundo antes de muestrear CPU durante dos segundos. El profiler recoge
Private Commit de host Rust, Renderer, GPU y total; Working Set y CPU. La
ruta local de estadísticas registra entradas y bytes binarios retenidos en
ImageAssets, y tamaño del corpus. No son memoria de bitmaps del motor.

Las series iniciales sin prefijo `paint-` llamaban `img.decode()` para esperar
la imagen. Esa operación no existe en el recorrido normal JPEG de producción;
puede cambiar la decodificación/caché y elevó notablemente el consumo del banco.
Se conservan como ensayos de decodificación forzada, **excluidos de la
conclusión sobre carga normal**. Tampoco se mezclan con las series nuevas.
El intento `final-incomplete` falló ante el cambio legítimo GIF/poster y se
excluye. Un desajuste de gaps del atlas experimental se corrigió antes de
las series `paint-*`, ocultando explícitamente el elemento `source`.

La serie separada `first_high_resolution` usa proceso y perfil nuevos, sin
las escenas anteriores. Registra diferencias de CPU acumulada por proceso
entre la selección y la carga; incluye la ventana temporal de las consultas
WMI. Son segundos de CPU de un núcleo, no el porcentaje de reposo. No mide
el pico transitorio de memoria de los decodificadores.

**Alcance:** mediciones reales de ese componente en Windows/WebView2, no del
cliente completo, transferencias de red o experiencia manual de chat/DM.
El visor y cambios de canal/DM son transiciones deterministas del banco.
No se presentan como navegación real de una cuenta. La instancia de usuario
preexistente se conserva y queda fuera de los PIDs medidos.

El atlas de avatares/emojis del banco usa URLs locales de ImageAssets.
No mide el transporte inline/remoto específico de `EmojiText`; ese recorrido
se audita en código. Los DMs del banco parten de miniaturas ya disponibles,
no de descargas/descifrado. Las diferencias en los escenarios posteriores a
fotos grandes pueden incluir la caché del motor de escenas anteriores;
no prueban una optimización independiente de fondos, GIF o avatares.

**VRAM:** no se obtiene una atribución fiable de VRAM en este banco. Private
Commit del proceso GPU mide memoria privada de CPU de ese proceso; no es
VRAM. Working Set agregado puede contar páginas compartidas varias veces.
No se convierten dimensiones de imágenes en una supuesta medición gráfica.

Los resultados compactos se escriben después de cada muestra en
`docs/RAM_PHASE8_RESULTS.json`; logs y capturas quedan bajo
`target/image-memory/`.

## Optimización 1 — copia binaria ligada a las vistas

`ImageSource` comparte un lease por recurso visible. Al cerrar la última
vista se suelta `AssetEntry.decoded`; se conserva el índice y la misma URL
mientras el owner original siga en MediaCache. Una respuesta tardía no vuelve
a llenar la caché sin vistas. Un finalizador antiguo no borra el buffer de
una generación nueva. No se cambia ningún byte de imagen ni el cache-control.

Resultados concluyentes `paint-*`, cinco procesos independientes por variante.
Mediana [mínimo–máximo]; desviación estándar muestral (SD):

| Métrica | Original | Lease |
|---|---:|---:|
| Rust al desmontar todo, MiB | 143,12 [142,12–143,75]; SD 0,64 | 94,49 [93,34–95,88]; SD 0,90 |
| Binarios retenidos al desmontar, bytes | 50.806.393 | 0 |
| Buffers / entradas al desmontar | 62 / 62 | 0 / 62 |
| Rust tras cambios de canal, MiB | 127,45 [127,16–129,16]; SD 0,83 | 78,49 [78,34–79,29]; SD 0,43 |
| Total al desmontar, MiB | 604,56 [589,20–620,95]; SD 11,23 | 545,54 [513,86–551,93]; SD 15,20 |
| CPU total tras cambios de canal, % | 0,00 [0,00–0,05]; SD 0,03 | 0,00 [0,00–0,05]; SD 0,02 |
| Seis alternancias de canal, ms | 141,88 [140,67–143,02]; SD 0,86 | 143,58 [142,14–145,57]; SD 1,34 |

Se conservan **48,63 MiB menos de Private Commit del host Rust** en el
desmontaje del banco y liberación exacta de 48,45 MiB de buffers binarios.
Las 62 entradas de metadatos permanecen porque el corpus simula owners vivos
en MediaCache: mantienen la URL, no los buffers. El corpus base64 residente
es idéntico: 75.479.580 bytes. Una futura solicitud puede volver a convertir
base64; la caché HTTP conserva la URL durante cinco minutos.
El total observado bajó 59,02 MiB, pero no se atribuye íntegramente al cambio:
incluye variación del motor/GPU. La latencia no mejoró; sus rangos se solapan.
Las variantes se ejecutaron en
bloques consecutivos, no intercaladas: existe posible deriva temporal del equipo.

Pasaron nueve tests del archivo real ImageAssets, sin ejecutar su benchmark
anterior: deduplicación, colisiones, límites, URL estable al reabrir, clones,
última vista, respuesta tardía, limpieza y concurrencia. Las respuestas siguen
usando el mismo payload, MIME y extensión de origen.

## Experimento descartado — derivadas locales JPEG

Se probó una derivada PNG sin pérdida adicional de codificación, hasta
640×480, exclusivamente para JPEG de 2–16 MP, con orientación y perfil ICC,
dos conversiones concurrentes y límites de dimensiones/asignación/salida.
El original se conservaba para el visor y para densidades superiores a 2×.
PNG/APNG/GIF/WebP/AVIF, avatares, DMs y fondos no se transformaban.
Reutilizaba ImageAssets y su lease; no creaba otra caché persistente.

| Métrica de carga normal | Base | Lease | Derivada descartada |
|---|---:|---:|---:|
| Chat 8 JPEG 1920×1280, total MiB | 256,12 | 257,71 | 249,20 |
| Chat, carga ms | 41,07 | 41,18 | 104,97 |
| 8 JPEG 4000×3000, Rust MiB | 118,13 | 111,52 | 83,93 |
| Alta resolución, Renderer MiB | 25,53 | 26,41 | 24,74 |
| Alta resolución, GPU Private MiB | 102,82 | 102,70 | 83,58 |
| Alta resolución, total MiB | 306,72 | 301,57 | 251,73 |
| Alta resolución, Working Set MiB | 476,81 | 469,23 | 414,91 |
| Alta resolución, carga ms | 167,20 | 167,56 | 383,69 |
| Primera carga aislada, total MiB | 286,61 | 288,82 | 250,91 |
| Primera carga aislada, carga ms | 168,11 | 168,46 | 291,69 |

CPU acumulada durante primera carga aislada, segundos de un núcleo;
mediana [mínimo–máximo], SD:

| Proceso | Base | Lease | Derivada descartada |
|---|---:|---:|---:|
| Rust | 0,1250 [0,1094–0,2188]; 0,0534 | 0,1094 [0,0938–0,2188]; 0,0557 | 0,5313 [0,5156–0,5469]; 0,0156 |
| Renderer | 0,3750 [0,3594–0,4063]; 0,0204 | 0,4063 [0,3438–0,4063]; 0,0271 | 0,0156 [0,0156–0,0313]; 0,0086 |
| GPU | 0,0313 [0,0156–0,0469]; 0,0131 | 0,0313 [0,0156–0,0781]; 0,0257 | 0,0156 [0,0000–0,0156]; 0,0086 |
| Árbol total, incluye auxiliares | 0,6719 [0,5625–0,7188]; 0,0694 | 0,6406 [0,5156–0,6875]; 0,0725 | 0,5938 [0,5781–0,6094]; 0,0131 |

No nacieron ni desaparecieron procesos entre las consultas de CPU de estas
quince corridas. Los rangos de CPU total se solapan: no hay reducción de CPU
demostrada. La derivada traslada trabajo al host. Ahorra 49,84 MiB en la
galería frente al lease, pero multiplica la latencia por 2,29; en chat común
ahorra sólo 8,51 MiB frente al lease, con rangos solapados y 2,55× de latencia.
No compensa su coste y complejidad. Se retiró todo su código de producción.

La regresión preliminar al restaurar, observada con `decode()` forzado, no
se reproduce en carga normal: total 350,87 / 304,75 / 290,06 MiB para
base / lease / derivada. No se intenta corregir producción para compensar
un efecto del banco. Las dimensiones naturales por sí solas no prueban que
Chromium retenga un bitmap completo; estos datos tampoco identifican su
estrategia interna exacta de decodificación.

El prototipo queda únicamente en `tools/image-memory-probe/thumbnail-experiment.patch`,
aplicable sobre `60b70f7`. La feature de banco `thumbnails` requiere aplicar
explícitamente ese parche en un checkout aislado. Está desactivada por
defecto; no forma parte del cliente ni de CI.

Tablas de **todos los escenarios**, Private Commit por proceso, Working Set,
CPU, latencia y bytes: [RAM_PHASE8_COMPARISON.md](RAM_PHASE8_COMPARISON.md).
Estadísticas completas, incluidas entradas, buffers e imágenes:
[RAM_PHASE8_STATISTICS.json](RAM_PHASE8_STATISTICS.json).
Datos crudos y hashes: [RAM_PHASE8_RESULTS.json](RAM_PHASE8_RESULTS.json).
Las series preliminares se conservan, identificadas y excluidas de la decisión.

## Compatibilidad y alcance de las pruebas

| Comprobación | Resultado y alcance |
|---|---|
| Compilación del cliente real | `cargo test --locked -p discordia --lib --no-run --offline` completó; compila los tests de la biblioteca completa, no sólo el banco |
| ImageAssets | Nueve pruebas de deduplicación, límites, vida útil y concurrencia. El prototipo agrega otras dos; no se ejecutó el benchmark antiguo |
| MediaCache | Diez pruebas superadas: LRU/presupuesto, bytes compartidos, pendientes, disco y limpieza |
| DM cifrado | Catorce pruebas superadas: AES-GCM con vector NIST, alteraciones, privacidad NIP-17, reintentos, persistencia, caché cifrada, expiración y límites. Una prueba de upload externo quedó ignorada: requiere servidor Blossom elegido explícitamente |
| Cambio de DM | Dos pruebas superadas: libera imágenes de otros canales y preserva historia |
| Avatares/perfiles | Tres pruebas de recuperación/reconexión/invalidación y una de llegada de blobs/cambio de persona superadas |
| Recepción/almacenamiento de adjuntos | Cinco integraciones de servidor superadas: metadatos y dimensiones sobreviven a historia/búsqueda/reapertura; permisos por nombre, respuesta deduplicada y escritura limitada al bundle solicitado |
| E2EE | Siete pruebas Rust de claves/epochs/cancelación y el script de workers reales superados |
| Video/pantalla | Cuatro pruebas de ciclo de vida; bridge JS y sus cancelaciones; veinte pruebas de calidad de transmisión superadas |
| WebRTC nativo | La regresión con SFU local y receptor real pasó H.264/NVIDIA, VP8/libvpx y H.264/OpenH264 a 1920×1080/60. No es una llamada manual ni mide captura de escritorio/cámara física |
| Prototipo de miniaturas | Tres pruebas de tamaño, orientación, originales y bypass de animaciones; dos de separación de recursos/bytes compartidos superadas. La variante previa con `srcset` falló la selección esperada a 3× y se descartó |
| Manual pendiente | Envío/recepción y actualización de perfil desde dos cuentas reales, Blossom externo, llamada de voz/cámara/pantalla/E2EE entre usuarios, cambio físico de monitor/DPI y QA perceptual con fotos reales |

No hubo fallos en las regresiones de producto ejecutadas. Las pruebas del
prototipo y los problemas del banco se identifican por separado. No se
afirma haber ejecutado manualmente la aplicación ni los casos pendientes.
La cobertura automática de fuente/visor/GIF/desmontaje en WebView2 queda
registrada en `RAM_PHASE8_CHECKS.json`.

Las capturas automáticas de alta resolución, atlas, DMs, visor y fondo
coinciden píxel a píxel entre base y lease. Chat difiere en promedio
0,043/255 por canal, máximo 4/255; no se infiere una causa interna del motor.
Los bytes originales no cambian. Datos en `RAM_PHASE8_VISUAL_CHECKS.json`.

## Reproducción

El banco normal incluye el ImageAssets de producción. `IMAGE_PROBE_ASSETS_SOURCE`
permite compilar un snapshot del mismo archivo sin cambiar de rama ni
sobrescribir el cliente. Se verificó la compilación del snapshot `b753f11`
y del estado conservado. Todos los comandos se ejecutan desde el repositorio:

```powershell
cargo build --manifest-path tools/image-memory-probe/Cargo.toml --target-dir target --release --locked
& target/release/DiscordiaImageProbe.exe generate target/image-memory/fixtures
node -e "require('fs').writeFileSync('target/image-memory/baseline-source.rs',require('child_process').execFileSync('git',['show','b753f11:client/src/image_assets.rs']))"
$env:IMAGE_PROBE_ASSETS_SOURCE = (Resolve-Path target/image-memory/baseline-source.rs).Path
cargo build --manifest-path tools/image-memory-probe/Cargo.toml --target-dir target --release --locked
Copy-Item target/release/DiscordiaImageProbe.exe target/image-memory/ReproBaseline.exe
Remove-Item Env:\IMAGE_PROBE_ASSETS_SOURCE
cargo build --manifest-path tools/image-memory-probe/Cargo.toml --target-dir target --release --locked
Copy-Item target/release/DiscordiaImageProbe.exe target/image-memory/ReproLease.exe
node scripts/measure-image-memory.cjs --variant paint-repro-baseline --exe target/image-memory/ReproBaseline.exe --runs 5
node scripts/measure-image-memory.cjs --variant paint-repro-lease --exe target/image-memory/ReproLease.exe --runs 5
node scripts/check-image-memory.cjs
```

Repetir las dos mediciones con `--load-cpu` para la serie de primera carga.
Los nombres nuevos preservan las mediciones originales. El script conserva
intentos incompletos y no los sobrescribe. El corpus requiere el mismo lockfile
e image 0.25.10 para comparar hashes. Para el prototipo descartado, comprobar
y aplicar su parche sólo en un checkout aislado y habilitar `--features thumbnails`.
No aplicarlo al cliente conservado como una optimización aprobada.

## Estado final y detención

Por instrucción posterior del usuario, toda la Fase 8 pasa al nuevo branch
local **`perf/phase-8-image-memory`**. `perf/ram-optimization` vuelve a
`b753f11`, sin perder la Fase 7. El remoto de esa rama seguía en `79ffc91`.

| Commit de Fase 8 | Contenido |
|---|---|
| `cff0589` | Banco Windows/WebView2 e instrumentación inicial |
| `60b70f7` | Único cambio de producción conservado: leases de ImageAssets |
| Commit de cierre `test(images): finalize Phase 8 audit and comparisons` | Metodología corregida, resultados finales, regresiones, reproducción y parche descartado |

Producción modificada: sólo `client/src/image_assets.rs`. Instrumentación:
`scripts/measure-ui-memory.ps1`, `measure-image-memory.cjs`, `check-image-memory.cjs`,
`summarize-image-memory.cjs` y `tools/image-memory-probe/`. Informe y evidencia:
`docs/RAM_PHASE8*` y entrada de `docs/MAP.md`.

Los helpers de medición y comprobación cerraron sus procesos y descendientes;
el SFU de regresión terminó. Se preservó la instancia de usuario PID 10772.
No se hizo push, ni se modificaron CI, releases, instaladores o la rama
principal. No se auditó ni modificó el globo.

Quedan como oportunidades, **sin implementar ni iniciar otra fase**: un banco
de galerías reales PNG/APNG y fotos con perfiles de color; contabilización de
posters GIF por elemento; fondos de muchos píxeles; comportamiento animado
WebP y validación completa con cuentas/llamadas reales. Antes de otra derivada
hay que demostrar beneficio sobre la decodificación normal del motor, no
sobre `naturalWidth` ni una espera artificial de decodificación.
