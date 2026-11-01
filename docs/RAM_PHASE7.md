# RAM — Fase 7, Etapa 4

## Referencia y alcance

Auditoría del 31 de octubre de 2026 (Argentina). GitHub confirmó
`perf/ram-optimization` en `79ffc913f73e3f0c578f00125d565bb9cd76a4ef`.
El checkout partió limpio de ese commit. La base remota real es `master`
(`3c0779d` al iniciar la auditoría); no existe `main`. Ese estado remoto agrega 19 commits,
con 22 archivos y +2947/-116 líneas respecto de esa base.

Al retomar el cierre el 1 de noviembre, GitHub confirmó que la rama de
optimización seguía en `79ffc91`, mientras `master` había avanzado a
`0136450`. Esta etapa no modificó `master` ni incorporó ese avance: las
mediciones comparan únicamente los dos perfiles del código auditado.

| Antecedente | Evidencia existente, sin repetir benchmarks |
|---|---|
| Etapa 1 | `a8f7026`: LiveKit y fuente del worker E2EE diferidos hasta conectar medios; `854a027`: suite WebView2. `target/statistical_runs.json`: 5 muestras por variante; mediana en reposo 303,56 → 288,65 MiB, diferencia 14,91 MiB; medias 305,528 → 291,218 MiB, diferencia 14,31 MiB |
| Etapa 2 | `target/stage2_flags_summary.json`: medianas Control 291,18, Translate 296,00, MediaRouter 291,52 y OptimizationHints 294,86 MiB. No se adoptaron estos flags |
| Etapa 3 | `target/stage3_dialogs_summary.json`: Control 298,78 y NoScriptDialogs 291,70 MiB de mediana, sin cambio de producción adoptado. La auditoría previa queda cerrada |
| Último commit | `79ffc91` conserva argumentos WebView2 externos y añade el argumento base existente; no activa por defecto los flags experimentales de Etapa 2 |

Los archivos históricos de `target/` son evidencia local preexistente, no datos
publicados en esos commits. Sus resúmenes auxiliares tienen campos en cero
para algunas métricas; no se reutilizan como mediciones de Etapa 4.

## Configuración comparada

| Propiedad | Release | release-lto |
|---|---|---|
| `opt-level` | 3 | 3, heredado |
| `lto` | `false`: ThinLTO local entre unidades del mismo crate | `"thin"`: entre crates Rust |
| `strip` | `"none"` | `"debuginfo"` |
| `debug` | 0 | 0 |
| `codegen-units` | 16 | 16 |
| `panic` | `"unwind"` | `"unwind"` |
| CRT | estática, `.cargo/config.toml` | igual |
| Features | `desktop`, predeterminadas | igual |

Valores heredados según [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html).
El candidato combina ThinLTO y stripping: una diferencia del ejecutable o de
memoria no identifica por sí sola el efecto causal de ThinLTO. El SDK C++
WebRTC precompilado no recibe optimización LTO cruzada Rust/C++.

## Método Windows

Windows 11 Pro, build 26300; AMD Ryzen 7 9700X, 16 procesadores lógicos,
31,16 GiB de RAM; Rust/Cargo 1.99.0, LLVM 23.1.1, MSVC 14.44.35207,
target `x86_64-pc-windows-msvc`, Node 24.22.1.

`node scripts/measure-thinlto.cjs` reutiliza `measure-ui-memory.ps1` con
`-TargetPid` y `-DescendantsOnly`. La selección por descendencia evita sumar
WebViews de otras sesiones. Una instancia de desarrollo inicialmente abierta
bloqueaba los lanzamientos mediante el mutex global de la aplicación; el
usuario autorizó su cierre temporal. Ya había salido al verificar su PID,
por lo que no fue necesario terminarla por fuerza.

- Cinco procesos independientes por perfil, orden R/L, L/R, R/L, L/R, R/L.
- Configuración y datos WebView2 nuevos por lanzamiento; pantalla inicial
  sin identidad, misma ventana y mismo directorio de trabajo.
- CDP local idéntico en ambas variantes; arranque desde creación del proceso
  hasta `#main[data-ready]` (DOM y fuentes listos, observado mediante polling).
  Incluye la instrumentación; no equivale a tiempo de primera pintura.
- Espera de 15 s desde interfaz lista, seguida de muestra de CPU de 5 s.
  Sin compilaciones o pruebas propias simultáneas durante el A/B.
- Private Commit y Working Set sumados por proceso. Working Set incluye
  páginas compartidas y puede contarlas más de una vez. CPU normalizada
  a los 16 procesadores lógicos, con redondeo del profiler por proceso.
- No se vacían cachés del sistema operativo: son reinicios de proceso,
  no arranques fríos de disco. La actividad del equipo sigue siendo una fuente
  de variación; cinco muestras no prueban equivalencia.
- Se solicita cierre de ventana y, si queda en la bandeja, se termina sólo
  el árbol propio. Se verifican PID y fecha de creación para evitar reutilización.

SDK nativo reutilizado: WebRTC `webrtc-89d790b`; SFU `1.13.9`, SHA-256
`4699597551a8a0a34beef0004a6c693082459c0dd8bcad264b2a453a1ee6f400`.
El SFU copiado a la caché de ThinLTO es idéntico al de Release.
No se usó `LIVEKIT_BUNDLE_SKIP`.

Orden de trabajo: builds de producción, A/B completo y luego compilación y
ejecución de tests. Tamaños y hashes registrados corresponden a los binarios
del A/B antes de los tests. Cargo puede regenerar el ejecutable del perfil
al compilar integraciones con dev-dependencies; para reproducir el A/B se
ejecutan primero los dos comandos de build indicados al final.

## Resultados A/B

WebView2 `156.0.4314.58`, ocho procesos por lanzamiento. Los diez snapshots
de interfaz coinciden en texto y dimensiones. Un intento previo salió por
el mutex de instancia única y se excluyó: no produjo una muestra.

Medianas; rango mínimo–máximo entre paréntesis. MiB = 1.048.576 bytes.

| Métrica | Release, n=5 | release-lto, n=5 | Diferencia de medianas L−R |
|---|---:|---:|---:|
| Private Commit total, MiB | 196,06 (194,75–201,99) | 200,60 (195,23–206,42) | +4,54 |
| Working Set total, MiB | 393,32 (391,34–409,86) | 407,97 (391,46–421,89) | +14,65 |
| CPU total normalizada, % | 0,86 (0,77–0,93) | 0,73 (0,58–0,85) | −0,13 puntos |
| Arranque observado, ms | 562,48 (516,51–623,09) | 591,04 (574,20–815,21) | +28,56 |
| Ejecutable, bytes | 109.267.968 | 113.718.784 | +4.450.816 (+4,07 %) |
| Build exitoso con caché, s | 89,12 | 268,46 | No comparable como build limpio |

| Variante / corrida | Private MiB | WS MiB | CPU % | Arranque ms |
|---|---:|---:|---:|---:|
| Release 1 | 200,93 | 400,72 | 0,93 | 516,51 |
| Release 2 | 201,99 | 409,86 | 0,90 | 623,09 |
| Release 3 | 194,75 | 391,34 | 0,86 | 559,18 |
| Release 4 | 195,30 | 393,32 | 0,81 | 578,21 |
| Release 5 | 196,06 | 393,11 | 0,77 | 562,48 |
| release-lto 1 | 200,60 | 421,89 | 0,85 | 815,21 |
| release-lto 2 | 202,05 | 412,26 | 0,65 | 607,55 |
| release-lto 3 | 195,23 | 391,46 | 0,58 | 591,04 |
| release-lto 4 | 206,42 | 406,38 | 0,73 | 577,93 |
| release-lto 5 | 200,03 | 407,97 | 0,80 | 574,20 |

Private Commit medio ± desviación estándar muestral: Release 197,806 ± 3,389
MiB; candidato 200,866 ± 4,024 MiB. Los rangos de memoria, CPU y arranque se
solapan. La menor CPU observada no establece una mejora estable atribuible
a ThinLTO, ni compensa una reducción de RAM que no apareció.

| Host Rust únicamente, mediana (rango) | Release | release-lto |
|---|---:|---:|
| Private Commit, MiB | 10,12 (9,86–10,39) | 10,14 (9,82–10,18) |
| Working Set, MiB | 36,55 (34,78–36,91) | 36,62 (34,74–51,31) |
| CPU normalizada redondeada, % | 0,00 (0,00–0,08) | 0,00 (0,00–0,00) |

Tampoco aparece ahorro en el host Rust. La CPU medida proviene principalmente
de WebView2, cuyos binarios no reciben ThinLTO de Cargo.

El aumento en disco se concentra en `.text`: 66.036.224 → 70.318.592 bytes
raw. El directorio de debug PE mide 84 bytes en ambos; eso no demuestra que
`strip` sea inerte ni identifica todos los símbolos externos. No se compiló
un tercer perfil para aislar `strip`: la conclusión corresponde al candidato
combinado existente, no a ThinLTO puro. Los PDB no integran el tamaño del exe.

ThinLTO tuvo dos intentos fallidos de 81,43 y 40,30 s (descargas sin DNS y
fallo de `lib.exe` en aislamiento), antes del build exitoso de 268,46 s con
cachés nativas reutilizadas y MSVC fuera del aislamiento. Total registrado
de los tres intentos: 390,19 s. Release reutilizó muchas más dependencias
previas; estos tiempos no cuantifican el sobrecosto aislado de LTO.

## Pruebas

| Verificación | Release | release-lto |
|---|---|---|
| WebView2 real, suite de ocho escenarios | Pasa | Pasa |
| Audio nativo E2EE + RED, con claves coincidentes e incompatibles | Pasa | Pasa |
| Video E2EE hasta decodificador real: H.264 NVIDIA, VP8 y H.264 CPU | Pasa | Pasa |
| Audio de pantalla: selección, deselección y reselección | Falla por timeout en reselección | Mismo fallo |
| Compilación de ejecutables de tests, `-j 2` | 399,54 s | 1354,87 s |

En el caso fallido se decodifica el tono inicial y se completa la
deselección. La reselección termina en `screen audio selection did not
complete: Elapsed(())`, en `client/tests/live_sfu.rs`, helper
`selective_audio_event`. Ambos perfiles muestran el mismo fallo; no es
evidencia de una regresión específica de ThinLTO. La causa no quedó determinada.
No se modificó producción para alterar ese resultado.

Los tests nativos usan tonos y cuadros generados, SFU real y decodificación
real. La suite WebView2 usa el motor real, ventanas principal/popout y
señalización simulada. No equivalen a una llamada manual con micrófono,
cámara o captura física del escritorio.

También pasaron `node client/tests/screenshare_bridge.cjs`,
`node client/tests/e2ee_worker.cjs` y los 20 casos de
`node --test scripts/test-stream-quality.mjs`. Este último requería corregir
el selector desactualizado del literal Rust, sin cambiar su contenido.

Se compiló una vez por perfil el conjunto de tests con:

```powershell
cargo test --locked -p discordia --profile PROFILE -j 2 --lib --test live_sfu --test webview2_multimedia_suite --no-run
```

Luego se ejecutaron los binarios generados, filtrando cada caso, con
`--test-threads=1`; los tres casos nativos requieren `--ignored`.
Estos tiempos incluyen dev-dependencies y el binario adicional que Cargo
construye para las integraciones; no se mezclan con los builds del A/B.

## Decisión

**Conservar Release como perfil predeterminado y `release-lto` como candidato
optativo existente.** No se encontró reducción de RAM en este escenario; el
exe creció y el arranque mediano fue algo mayor. Cinco muestras de una pantalla
sin identidad no permiten extrapolar a llamadas, cargas sostenidas o todos
los equipos Windows. No se cambia configuración de producción, CI, releases
ni instaladores.

## Archivos y reproducción

| Archivo | Cambio de Etapa 4 |
|---|---|
| `docs/RAM_PHASE7.md` | Auditoría, método, resultados y decisión |
| `docs/RAM_PHASE7_RESULTS.json` | Datos por corrida, binarios, builds y resultados de tests |
| `docs/MAP.md` | Entrada del experimento y rutas actuales |
| `docs/OPS.md` | Comando y enlace al informe completo |
| `scripts/measure-thinlto.cjs` | Lanzamientos alternados, interfaz lista por CDP, aislamiento y cierre de árboles propios; conserva resultados para reanudar sin repetir muestras |
| `scripts/measure-ui-memory.ps1` | Opción `-DescendantsOnly`, sin cambiar el comportamiento predeterminado |
| `scripts/test-stream-quality.mjs` | Selector del literal `SCREEN_JS` independiente de su visibilidad Rust; el selector anterior dejó de encontrarlo al pasar de `pub(crate)` a `pub` |

[Datos registrados](RAM_PHASE7_RESULTS.json). Los JSON originales por proceso,
logs de build/tests y verificaciones de cierre se generaron en
`target/thinlto-stage4/`, ignorado por Git. Al retomar el cierre el 1 de
noviembre de 2026, ese directorio ya no estaba disponible. Se conservan los
datos compactos completos: diez muestras, hashes/tamaños, tiempos de build,
estadísticas y resultados de tests. No se repitieron mediciones para
reemplazar los logs ausentes.

Desde la raíz, con los SDK nativos disponibles:

```powershell
cargo build --locked -p discordia --bin Discordia --release
cargo build --locked -p discordia --bin Discordia --profile release-lto
node scripts/measure-thinlto.cjs
```

Cerrar instancias existentes antes del A/B: el mutex global impide ejecutar
dos clientes, aunque sus directorios de configuración sean diferentes.
Para otra evaluación, preservar o renombrar el directorio de resultados
anterior; el script evita sobrescribir muestras completas o intentos fallidos.

## Resumen de Fase 7

| Etapa | Resultado |
|---|---|
| 1 — evaluación diferida de LiveKit/E2EE | Presente y conservada; ahorro histórico de mediana 14,91 MiB en reposo |
| 2 — flags Chromium | Cerrada; no adoptados |
| 3 — configuración WebView2 | Auditoría cerrada; sin modificación de producción adoptada |
| 4 — ThinLTO | A/B Windows real, cinco lanzamientos por perfil; no adoptar como predeterminado |

Las Etapas 1–3 no se volvieron a ejecutar. Esta evaluación no inicia la Fase 8.

Los procesos de medición y tests finalizaron. Se reabrió la instancia de
desarrollo que se había autorizado cerrar temporalmente; las compilaciones
de otras sesiones se preservaron.
