# DM y comunidades — mediciones

Real production image preparation functions and Windows/WebView2 component fixture; excludes network, cryptography and whole-client state. GPU private memory is not VRAM.

Cinco procesos por variante, ocho imágenes visibles y perfiles nuevos. Medianas; MiB = 1048576 bytes. La carga parte de los recursos preparados en RAM y termina tras complete y dos frames; no fuerza img.decode().

| Variante | Rust MiB | Renderer MiB | GPU Private MiB | Total MiB | Working Set MiB |
|---|---:|---:|---:|---:|---:|
| jpeg-medium-server | 21.19 | 22.70 | 88.12 | 194.52 | 360.88 |
| jpeg-medium-dm | 5.88 | 26.32 | 79.12 | 171.04 | 341.52 |
| jpeg-large-server | 82.00 | 23.95 | 91.47 | 256.16 | 433.63 |
| jpeg-large-dm | 6.56 | 26.15 | 80.62 | 172.42 | 342.94 |
| jpeg-large-server-2mib | 16.31 | 25.27 | 93.23 | 192.73 | 361.39 |
| png-alpha-server | 7.27 | 24.79 | 76.06 | 168.44 | 357.53 |
| png-alpha-dm | 5.95 | 23.42 | 76.21 | 164.81 | 336.47 |

| Variante | Carga ms [mín–máx]; SD | CPU de carga, segundos de núcleo [mín–máx]; SD | CPU posterior % [mín–máx]; SD |
|---|---:|---:|---:|
| jpeg-medium-server | 41.75 [40.88–42.05]; SD 0.49 | 0.16 [0.11–0.17]; SD 0.03 | 0.00 [0.00–0.10]; SD 0.04 |
| jpeg-medium-dm | 19.42 [18.89–19.70]; SD 0.37 | 0.03 [0.02–0.08]; SD 0.03 | 0.05 [0.00–0.14]; SD 0.06 |
| jpeg-large-server | 167.67 [165.59–172.99]; SD 2.91 | 0.56 [0.55–0.73]; SD 0.08 | 0.00 [0.00–0.05]; SD 0.03 |
| jpeg-large-dm | 19.29 [18.46–27.78]; SD 4.51 | 0.06 [0.03–0.08]; SD 0.02 | 0.00 [0.00–0.05]; SD 0.02 |
| jpeg-large-server-2mib | 36.26 [34.75–37.04]; SD 0.96 | 0.19 [0.13–0.25]; SD 0.05 | 0.10 [0.00–0.24]; SD 0.11 |
| png-alpha-server | 27.39 [26.47–27.55]; SD 0.46 | 0.14 [0.08–0.22]; SD 0.05 | 0.05 [0.00–0.15]; SD 0.08 |
| png-alpha-dm | 19.27 [18.62–19.66]; SD 0.42 | 0.03 [0.00–0.22]; SD 0.09 | 0.00 [0.00–0.05]; SD 0.02 |

Preparación: media por imagen dentro de cada lote, mediana de cinco procesos. Tiempo de pared; no equivale a CPU ni al tiempo total de recepción concurrente. La lectura para comunidad fue primero, luego DM: puede haber calentamiento; no se atribuye su diferencia al recorrido. No es el envío completo. DM frío incluye validación/decodificación y generación de miniatura; excluye descarga, hashes, AES y disco cifrado.

| Caso | Leer/validar para adjuntar comunidad ms | Leer/validar para adjuntar DM ms | Trabajo de imagen al recibir DM frío ms |
|---|---:|---:|---:|
| jpeg-medium | 12.72 [12.53–12.88]; SD 0.14 | 9.75 [9.66–9.84]; SD 0.07 | 28.81 [28.66–28.91]; SD 0.10 |
| jpeg-large | 49.22 [48.98–49.28]; SD 0.13 | 46.08 [45.61–46.24]; SD 0.27 | 110.27 [109.82–110.61]; SD 0.30 |
| png-alpha | 7.27 [7.19–7.41]; SD 0.08 | 4.40 [4.38–4.42]; SD 0.02 | 13.07 [12.90–13.14]; SD 0.11 |
| gif-large | 2.24 [1.83–2.48]; SD 0.25 | 1.27 [1.21–1.50]; SD 0.12 | 4.55 [4.42–4.85]; SD 0.16 |
| jpeg-exif | 13.52 [13.35–14.13]; SD 0.30 | 9.84 [9.73–9.88]; SD 0.06 | 29.64 [29.48–29.70]; SD 0.09 |

| Caso, imagen 0 | Fuente | Comunidad por defecto | Miniatura DM |
|---|---|---|---|
| jpeg-medium | 1920×1280; image/jpeg; 867602 bytes; 1 frame(s); NoTransforms | 1920×1280; image/jpeg; 867602 bytes; 1 frame(s); NoTransforms | 640×427; image/jpeg; 31972 bytes; 1 frame(s); NoTransforms |
| jpeg-large | 4000×3000; image/jpeg; 4191348 bytes; 1 frame(s); NoTransforms | 4000×3000; image/jpeg; 4191348 bytes; 1 frame(s); NoTransforms | 640×480; image/jpeg; 56993 bytes; 1 frame(s); NoTransforms |
| png-alpha | 2560×1440; image/png; 97103 bytes; 1 frame(s); NoTransforms | 2560×1440; image/png; 97103 bytes; 1 frame(s); NoTransforms | 640×360; image/png; 24217 bytes; 1 frame(s); NoTransforms |
| gif-large | 960×640; image/gif; 20147 bytes; 8 frame(s); NoTransforms | 960×640; image/gif; 20147 bytes; 8 frame(s); NoTransforms | 640×427; image/png; 6327 bytes; 1 frame(s); NoTransforms |
| jpeg-exif | 1920×1280; image/jpeg; 867638 bytes; 1 frame(s); Rotate90 | 1920×1280; image/jpeg; 867638 bytes; 1 frame(s); Rotate90 | 640×427; image/jpeg; 31972 bytes; 1 frame(s); NoTransforms |

Todos los recursos de comunidades por defecto y los originales enviados por DM conservaron sus bytes en este corpus: true. Política reducida de 2 MiB: caso separado, no el valor por defecto.
