# Política común — mediciones

Real Dioxus/ImageAssets/WebView2 component fixture; not full Discordia or network transfer; GPU process private commit is not VRAM

Windows/WebView2, cinco procesos por variante; ocho imágenes, 1440×900, DPR 1, ventana visible y enfocada, perfil nuevo. Variantes intercaladas; orden invertido en rondas pares. Sin compilaciones paralelas. Se conservó Discordia PID 10772 de otra sesión.

Carga desde recursos preparados hasta complete y dos frames; sin forzar img.decode(). Excluye red, cifrado, estado del cliente completo y generación fría en servidor. GPU Private Commit no es VRAM. Working Set es la suma por proceso.

| Variante | Rust MiB | Renderer MiB | GPU Private MiB | Total MiB | Working Set MiB |
|---|---:|---:|---:|---:|---:|
| jpeg-medium-community-before | 21.13 | 23.75 | 89.55 | 193.47 | 363.06 |
| jpeg-medium-dm-before | 6.50 | 26.21 | 79.12 | 172.51 | 343.36 |
| jpeg-medium-unified | 6.91 | 26.87 | 79.14 | 172.13 | 342.88 |
| jpeg-large-community-before | 82.00 | 23.45 | 91.46 | 256.25 | 432.83 |
| jpeg-large-dm-before | 6.50 | 27.38 | 80.56 | 174.20 | 343.05 |
| jpeg-large-unified | 8.21 | 26.38 | 80.54 | 174.27 | 344.37 |
| png-alpha-community-before | 7.22 | 24.84 | 76.24 | 169.09 | 359.17 |
| png-alpha-dm-before | 6.07 | 26.47 | 75.97 | 169.00 | 341.24 |
| png-alpha-unified | 6.13 | 26.12 | 76.07 | 167.03 | 342.06 |

| Variante | Total MiB [mín–máx]; SD | Carga ms [mín–máx]; SD | CPU de carga, segundos de núcleo [mín–máx]; SD | CPU posterior % [mín–máx]; SD |
|---|---:|---:|---:|---:|
| jpeg-medium-community-before | 193.47 [193.24–205.60]; SD 5.32 | 41.42 [40.44–42.15]; SD 0.68 | 0.17 [0.13–0.19]; SD 0.03 | 0.00 [0.00–0.00]; SD 0.00 |
| jpeg-medium-dm-before | 172.51 [170.66–182.20]; SD 4.75 | 19.11 [18.63–19.36]; SD 0.27 | 0.03 [0.02–0.13]; SD 0.05 | 0.00 [0.00–0.00]; SD 0.00 |
| jpeg-medium-unified | 172.13 [170.43–184.72]; SD 5.85 | 27.96 [27.11–29.80]; SD 1.07 | 0.08 [0.03–0.16]; SD 0.06 | 0.00 [0.00–0.00]; SD 0.00 |
| jpeg-large-community-before | 256.25 [254.85–267.82]; SD 5.40 | 172.58 [167.25–175.52]; SD 3.90 | 0.56 [0.52–0.69]; SD 0.07 | 0.05 [0.00–0.43]; SD 0.18 |
| jpeg-large-dm-before | 174.20 [173.72–183.16]; SD 4.03 | 27.01 [18.69–29.36]; SD 4.12 | 0.06 [0.02–0.11]; SD 0.03 | 0.00 [0.00–0.10]; SD 0.04 |
| jpeg-large-unified | 174.27 [172.71–180.59]; SD 3.03 | 28.25 [27.04–28.77]; SD 0.76 | 0.05 [0.02–0.06]; SD 0.02 | 0.00 [0.00–0.19]; SD 0.08 |
| png-alpha-community-before | 169.09 [167.69–179.33]; SD 4.89 | 27.57 [19.92–28.85]; SD 3.69 | 0.13 [0.03–0.16]; SD 0.06 | 0.05 [0.00–0.19]; SD 0.08 |
| png-alpha-dm-before | 169.00 [164.23–179.28]; SD 5.72 | 19.70 [19.07–20.13]; SD 0.46 | 0.02 [0.00–0.09]; SD 0.04 | 0.00 [0.00–0.14]; SD 0.07 |
| png-alpha-unified | 167.03 [164.86–181.70]; SD 6.84 | 19.12 [18.66–19.99]; SD 0.61 | 0.03 [0.00–0.09]; SD 0.04 | 0.00 [0.00–0.14]; SD 0.07 |

| Caso | Generación de miniatura, Release ms/imagen | Validación y miniatura DM frío, Release ms/imagen |
|---|---:|---:|
| jpeg-medium | 21.10 [20.69–21.42]; SD 0.28 | 30.04 [29.78–30.11]; SD 0.15 |
| jpeg-large | 68.10 [67.62–69.16]; SD 0.59 | 111.83 [111.69–112.98]; SD 0.59 |
| png-alpha | 8.84 [8.74–9.34]; SD 0.26 | 13.42 [13.07–13.59]; SD 0.24 |
| gif-large | 0.02 [0.01–0.02]; SD 0.00 | 1.18 [1.10–1.40]; SD 0.15 |
| jpeg-exif | 20.04 [19.66–20.18]; SD 0.20 | 29.54 [29.24–30.14]; SD 0.33 |

Preparación: media por imagen dentro de cada lote y mediana de cinco procesos. El orden fijo de preparación puede calentar el codec; sus diferencias no representan un A/B de servidor frente a DM. Excluye red, cifrado y caché del servidor.

MediaStore::preview en ejecutable de test Release: cinco procesos; mismo JPEG 4000×3000. Excluye subida, red, cola del semáforo y socket.

| Estado | ms [mín–máx]; SD |
|---|---:|
| Primera generación y escritura | 72.02 [71.17–72.34]; SD 0.57 |
| Lectura de miniatura cacheada | 3.41 [3.33–4.31]; SD 0.43 |

Las variantes unified usan recursos idénticos para comunidades y DMs; por eso se mide una sola variante común por formato, evitando duplicar el mismo escenario. Los PNG de DM anterior y unified son idénticos: sus diferencias pequeñas de consumo/carga se tratan como variabilidad. Los originales se verifican byte por byte antes del A/B.
