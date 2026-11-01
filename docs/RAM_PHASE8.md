# Fase 8 — imágenes y memoria gráfica

## Referencia y alcance

Rama `perf/ram-optimization`, árbol limpio al iniciar en `b753f11`.
GitHub confirmó `origin/perf/ram-optimization` en `79ffc91`: el commit de
Fase 7 seguía siendo local. No se repitieron sus benchmarks. No se audita
ni modifica el globo. No se modifican CI, instaladores, releases o protocolo.

## Auditoría del recorrido

| Recurso | Recepción, renderizado y liberación existentes | Archivos revisados |
|---|---|---|
| Adjuntos de servidor | Sentinel `media:<hash>.<ext>`; `FetchEmoji` entrega el original como data URL, con deduplicación por dirección y lectura de disco fuera del hilo UI. `ChatImage` usa URL local de `ImageAssets`; muestra el original con CSS máximo 320×240. El visor solicita el mismo original. No hay miniatura de red ni resolución adaptada en este recorrido | `server/src/gateway/connection.rs` (`FetchEmoji`), `client/src/net.rs` (`EmojiBlobs`, `request_missing_images`), `state.rs` (`media_src_shared`), `features/chat.rs` (`ChatImage`), `features/image_viewer.rs` |
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

Cada muestra espera las imágenes mediante `HTMLImageElement.decode()` y dos
frames; registra latencia de transición/decodificación observada y espera un
segundo antes de muestrear CPU durante dos segundos. El profiler recoge
Private Commit de host Rust, Renderer, GPU y total; Working Set y CPU. La
ruta local de estadísticas registra entradas y bytes binarios retenidos en
ImageAssets, y tamaño del corpus. No son memoria de bitmaps del motor.

**Alcance:** mediciones reales de ese componente en Windows/WebView2, no del
cliente completo, transferencias de red o experiencia manual de chat/DM.
El visor y cambios de canal/DM son transiciones deterministas del banco.
No se presentan como navegación real de una cuenta. La instancia de usuario
preexistente se conserva y queda fuera de los PIDs medidos.

**VRAM:** no se obtiene una atribución fiable de VRAM en este banco. Private
Commit del proceso GPU mide memoria privada de CPU de ese proceso; no es
VRAM. Working Set agregado puede contar páginas compartidas varias veces.
No se convierten dimensiones de imágenes en una supuesta medición gráfica.

Los resultados compactos se escriben después de cada muestra en
`docs/RAM_PHASE8_RESULTS.json`; logs y capturas quedan bajo
`target/image-memory/`.
