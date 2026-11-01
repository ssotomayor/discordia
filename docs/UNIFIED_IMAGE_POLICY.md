# Política común de imágenes y visor progresivo

Comunidades y DMs muestran la misma vista previa acotada y abren el original
en un visor progresivo. Comunidades: el servidor genera la miniatura. DMs: la
genera el receptor tras descifrar. Generarla en el emisor requiere otro diseño
de protocolo.

| Área | Implementación |
|---|---|
| Motor compartido | `image-policy/`; máximo 640×480, sin ampliar imágenes pequeñas. JPEG calidad 92; PNG conserva formato y alfa. Límites de entrada 10 MiB, 16 millones de píxeles, dimensiones 8192 y asignación del decoder 128 MiB. Derivada hasta 2 MiB |
| Color y animación | Aplica orientación EXIF a los píxeles; copia ICC RGB/GRAY acotado. GIF, APNG, WebP y AVIF conservan original. PNG con transferencias/primarias no estándar o metadatos HDR conserva original porque el encoder no los puede reproducir |
| Comunidades | `protocol/src/lib.rs`, `server/src/media.rs`, `server/src/gateway/connection.rs`, `client/src/net.rs`, `features/chat.rs`. Ready anuncia capacidad opcional, ausente=false; servidores antiguos conservan el recorrido original |
| Caché servidor | Sidecars versionados dentro de MediaStore, contados en la cuota. Subpresupuesto min(128 MiB, cuota/8); expulsa las más antiguas antes de rechazar una subida de originales. La referencia al original protege su derivada en el barrido |
| Saturación | Dos trabajos activos de servidor, 16 admitidos globalmente, hasta ocho por conexión y 64 nombres por solicitud. Sin cupo: respuesta vacía y reintento del cliente tras ocho segundos. Trabajos bloqueantes fuera del socket |
| Caché cliente | `media_cache.rs`; claves preview/original distintas en el mismo LRU de 128 MiB/2048 entradas desktop (64 MiB Android) |
| DMs | `nostr/dm_image.rs`, `features/dm_images.rs`, `nostr/service.rs`, `state.rs`. Caché cifrada `.preview-v2.thumb`. No cambia AES, NIP-17 ni Blossom. Miniaturas antiguas se ignoran sin borrar ciphertext ni mensajes |
| Presentación | `features/image_attachment.rs`, `image_assets.rs`; CSS máximo 320×240. Por encima de DPR 2 se usa el original |
| Visor progresivo | `features/image_viewer.rs`, `assets/image-viewer.js`, `image-viewer.css`. Abre con la miniatura; sustituye la fuente conservando instancia, zoom y desplazamiento. Fallos conservan miniatura y ofrecen reintento; espera 30 s; identidad de sesión e intento descarta respuestas tardías |

## Medir

En Windows: `scripts/measure-unified-images.cjs` (A/B, nueve variantes),
`summarize-unified-images.cjs`, `check-unified-images.cjs` (política) y
`check-progressive-images.cjs` (visor), con `tools/image-memory-probe`
(`--features unified-policy` / `progressive-viewer`). Salida en
`target/image-reports/`.
