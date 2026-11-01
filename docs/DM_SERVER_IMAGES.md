# Imágenes de DMs y comunidades

Línea base anterior a la unificación. La implementación y el A/B posterior
están en [UNIFIED_IMAGE_POLICY.md](UNIFIED_IMAGE_POLICY.md).

Comparación sobre `58ff9d3`, branch `perf/phase-8-image-memory`. Se reutilizan
las funciones reales de `chat_image.rs`, ImageAssets y el profiler Windows.
No modifica producción, política de archivos, protocolo ni datos del usuario.

## Resultado principal

Medianas de cinco ejecuciones independientes; ocho imágenes visibles por
lote, con miniaturas ya preparadas. MiB, no MB decimales:

| Fuente | Total comunidad | Total DM | Carga comunidad | Carga DM |
|---|---:|---:|---:|---:|
| JPEG 1920×1280 | 194,52 MiB | 171,04 MiB | 41,75 ms | 19,42 ms |
| JPEG 4000×3000 | 256,16 MiB | 172,42 MiB | 167,67 ms | 19,29 ms |
| PNG RGBA 2560×1440 | 168,44 MiB | 164,81 MiB | 27,39 ms | 19,27 ms |

Para JPEG grande: **83,74 MiB menos de Private Commit total** en este banco,
un 32,7%. Rust pasa de 82,00 a 6,56 MiB; Renderer de 23,95 a 26,15 MiB;
GPU Private de 91,47 a 80,62 MiB. El ahorro principal es del recurso retenido
en Rust, no una medición de VRAM ni prueba de un bitmap completo en GPU.
Working Set total: 433,63 → 342,94 MiB. CPU de carga: 0,5625 → 0,0625
segundos de un núcleo, incluyendo auxiliares; no es porcentaje instantáneo.

Ese DM necesita antes **110,27 ms por imagen** de validación y generación
de miniatura en recepción fría, sin red/cifrado/hashes/disco cifrado. El
equivalente es 28,81 ms para JPEG medio y 13,07 ms para PNG del corpus.
No se presenta 19,29 ms como tiempo completo de recibir un DM frío.

El total JPEG grande varió 255,48–267,65 MiB en comunidad (SD 5,09) y
169,26–181,79 MiB en DM (SD 4,96). PNG mostró poco ahorro de Private Commit
y rangos de CPU solapados: no se generaliza el resultado JPEG a todo PNG.
La CPU posterior de reposo también tiene rangos solapados.

Con política de comunidad reducida a 2 MiB, el JPEG grande se envía como
2048×1536, unos 597 KB para la imagen 0: total 192,73 MiB y carga 36,26 ms.
Normalizarlo costó 146,32 ms por imagen. **No equivale a una miniatura:**
modifica la versión enviada y la que puede recuperar el visor; pierde el
original de 4000×3000 en ese recorrido. No se recomienda reducir la política
de archivos como una optimización transparente de memoria.

## Recorridos reales

| Aspecto | Comunidad / servidor | DM Nostr |
|---|---|---|
| Límite de archivo | Configurable por comunidad; por defecto y máximo 10.000.000 bytes, unos 9,54 MiB | 10.485.760 bytes, 10 MiB |
| Selección desde archivo | `read_file_with_limit`; si entra, valida/decodifica y conserva bytes. Si no entra, intenta normalizarlo | Misma función, distinto límite |
| Normalización por exceso | Prueba máximos 2048, 1600 y 1280; JPEG con calidad 85, 75 y 65 hasta caber. Con alfa prueba PNG antes de JPEG. Rechaza GIF demasiado grande | Igual algoritmo antes de cifrar; no es la generación de miniaturas |
| Portapapeles | Genera PNG RGBA limitado; puede rechazar por tamaño. No debe confundirse con la normalización de un archivo | Mismo mecanismo, con el límite DM |
| Transporte | Sentinel `media:hash.ext`, blob en servidor y resolución `FetchEmoji` como data URL; demanda deduplicada | Original cifrado AES-GCM en Blossom; referencia en NIP-17, fuera del gateway |
| Primera recepción | Resuelve la versión enviada; no genera miniatura en Rust | Descarga original cifrado si no está en caché; valida y descifra; genera miniatura local y la guarda cifrada |
| Vista previa | Versión enviada original, escalada por CSS | Imagen hasta 640×480. JPEG con calidad por defecto 75 si no tiene alfa; PNG si tiene alfa. Si ya cabe en 640×480 conserva los bytes |
| CSS de ambas vistas | `max-width: min(320px, 82vw); max-height: 240px; width:auto; height:auto; object-contain` | Exactamente el mismo límite visual |
| Apertura completa | La versión enviada se obtiene de MediaCache; no recupera una fuente anterior a la normalización de envío | `load_original` recupera ciphertext de disco o descarga, valida y descifra el original enviado |
| Caché caliente | MediaCache mantiene data URLs con LRU, desktop 128 MiB / 2048 entradas | Reutiliza `.thumb` cifrado sin regenerarlo. Disco desktop 256 MiB, TTL siete días, máximo 256 archivos |
| RAM al abandonar | MediaCache puede conservar originales. ImageAssets suelta buffers binarios con la última vista | Al desaparecer viewers se suelta `data`; cambiar DM libera otras vistas, preservando historia. Política Ready de ocho, sin expulsar activas |

Una miniatura cambia el archivo y su resolución. CSS cambia su presentación.
Base64 es una codificación de transporte/almacenamiento, no una compresión
de imagen: agrega aproximadamente un tercio de bytes. El campo `decoded`
de ImageAssets contiene bytes comprimidos, no píxeles RGBA.

Los DMs no ahorran la descarga del original en una recepción fría sólo por
mostrar una miniatura: la crean localmente después de recibirlo. Sí reducen
el recurso mantenido en RAM y presentado al navegador después de procesarlo.

`decrypt_raw` valida la imagen mediante `from_bytes_with_limit`, que decodifica
y genera un data URL completo; luego `receive` llama `thumbnail_data_url`,
que vuelve a decodificar las imágenes grandes. Ese trabajo frío no ocurre
al reutilizar el `.thumb`. No se deduce su coste del tiempo de mostrar una
miniatura previamente preparada.

El emisor DM también valida al cifrar y prepara una miniatura para su caché
en `Job::prepare`. La lectura para adjuntar no incluye ese trabajo ni
representa el tiempo completo de enviar por ninguno de los recorridos.

## Banco y límites

Tres lotes de ocho imágenes: JPEG 1920×1280, JPEG 4000×3000 y PNG RGBA
2560×1440. Los mismos archivos entran por ambos recorridos. Se añade una
política de comunidad reducida a 2 MiB para JPEG grande; **no es el defecto**.
GIF de ocho frames y JPEG con EXIF Rotate90 son controles funcionales.

Cinco procesos independientes para preparar cada lote, después de generar
fixtures. Cinco procesos/perfiles WebView2 por variante, alternando comunidad
y DM dentro de cada repetición. Se mide carga desde datos preparados en RAM,
CPU acumulada del árbol durante la carga, Private Commit por proceso y total,
Working Set y CPU posterior. No se fuerza `img.decode()`.

No es una sesión completa con cuentas reales ni incluye latencia de Internet,
cifrado, hashing, SFU o escritura/lectura de caché cifrada. El tiempo frío de
procesamiento se mide separado con las funciones reales; no se suma a la
carga como si no hubiera concurrencia. No mide el pico transitorio de memoria
de la generación ni la latencia de abrir el original de un DM real.

La lectura para adjuntar se ejecutó primero para comunidad y luego para DM,
con la misma implementación cuando ambas conservan bytes. El calentamiento
de archivo/asignador puede explicar diferencias entre esas columnas; **no
se interpreta que seleccionar para DM sea más rápido** por su límite.
Las 35 mediciones de visualización sí alternan variantes entre repeticiones.

GPU Private Commit **no es VRAM**. El Working Set agregado puede contar
páginas compartidas varias veces. Los cambios de consumo reflejan también
que la comunidad conserva el original y el DM sólo la miniatura en RAM.
No son exclusivamente una diferencia de decodificador o de CSS.

## Calidad y diferencias comprobadas

En el corpus por defecto, la comunidad y el original DM mantienen bytes.
La miniatura JPEG se vuelve a codificar con calidad 75; la PNG conserva alfa
y usa codificación sin pérdida después de reducir resolución. Eso no garantiza
una imagen indistinguible a cualquier zoom/DPI: ambas vistas limitan tamaño
en CSS y el original se puede abrir aparte.

Una miniatura de 640 px cubre 320 px CSS hasta una densidad de 2×. No hay
selección `srcset`/DPR en este recorrido DM; no se validó calidad en monitor
3× ni con fotografías reales o perfiles ICC. No se afirma igualdad visual
universal entre una miniatura JPEG calidad 75 y el original.

En las capturas del corpus, PNG coincide píxel a píxel. JPEG grande tiene
MAE RGB 1,135/255 sobre la captura completa; JPEG medio 1,123/255, incluyendo
el cambio de proporción por redondeo a 640×427. Es comparación automática,
no validación perceptual. Datos en `DM_SERVER_IMAGE_VISUAL_CHECKS.json`.

Dos diferencias preexistentes reproducidas, sin corregirlas:

- GIF 960×640: comunidad conserva GIF con ocho frames; DM produce PNG 640×427
  con un solo frame en la miniatura. Abrir el original conserva la animación.
- JPEG EXIF Rotate90: comunidad conserva orientación y bytes. La miniatura DM
  elimina el metadato sin aplicar la rotación en `thumbnail_data_url`.

El control de GIF no se usa como A/B de rendimiento equivalente: animado y
estático ofrecen comportamientos distintos. La inspección de metadatos y
frames es automática; no se afirma haber probado manualmente cuentas reales.

## Evidencia y reproducción

[Tablas de consumo, carga, CPU y procesamiento](DM_SERVER_IMAGE_MEASUREMENTS.md).
[Estadísticas y cinco preparaciones](DM_SERVER_IMAGE_STATISTICS.json).
[Muestras del profiler y hashes](DM_SERVER_IMAGE_RESULTS.json).

El banco incluye directamente `client/src/chat_image.rs`; `arboard` sólo se
agrega al proyecto de herramientas para compilar esa función existente. No
se usa el portapapeles durante las pruebas. No se agrega dependencia al cliente.

```powershell
cargo build --manifest-path tools/image-memory-probe/Cargo.toml --target-dir target --release --locked
& target/release/DiscordiaImageProbe.exe generate target/image-memory/fixtures
& target/release/DiscordiaImageProbe.exe compare-dm-server target/dm-server-images 0
& target/release/DiscordiaImageProbe.exe compare-dm-server target/dm-server-images 1
node scripts/measure-image-memory.cjs --variant jpeg-large-server --exe target/release/DiscordiaImageProbe.exe --runs 5 --load-cpu --out target/dm-server-images/measurements --fixtures target/dm-server-images/jpeg-large/server --report docs/DM_SERVER_IMAGE_RESULTS.json
```

Repetir preparación con índices 2–5; usar directorios `dm`, `server` y
`server-2mib` según corresponda. Para alternar, ejecutar cada variante con
`--runs 1`, luego `--runs 2`, etc.; el script omite corridas completadas y
preserva intentos incompletos. Recalcular con
`node scripts/summarize-dm-server-images.cjs`. Las series de Fase 8 anteriores
se preservan en archivos separados.

Pasaron los nueve tests existentes de `chat_image::tests`, excluyendo benchmarks.
Todas las muestras tienen ocho imágenes cargadas; no nacieron/desaparecieron
procesos durante sus ventanas de CPU. Los helpers cerraron sus árboles de
procesos. Se registran resultados y herramientas en un commit local separado;
no se hace push sin nueva autorización ni se inicia otra fase.
