# Android: revisión y medición — 2026-10-23

Base: `5b853c4`, rama `mobile-platform-support`. Revisión de los adaptadores Android, captura nativa, cancelación y renderizado en reposo; no es una auditoría de todo el producto. Cambios anteriores conservados.

## Errores corregidos

| Prioridad | Fallo | Corrección / evidencia |
|---|---|---|
| P1 | Servicio `mediaProjection` iniciado antes del consentimiento; captura sin esperar que el servicio esté en primer plano | Consentimiento → servicio → confirmación por `ResultReceiver` → capturador. Orden exigido por [Android](https://developer.android.com/develop/background-work/services/fgs/service-types#media-projection). APK compila; consentimiento/captura física requieren otra prueba. |
| P2 | `toI420()` no se liberaba cuando devolvía el mismo buffer | Liberación incondicional de la referencia obtenida. `javap` sobre el **jar empaquetado** confirma `retain()` antes de devolver `this`; no se libera el `VideoFrame` prestado. |
| P2 | `Capture::stop(self)` detenía la cámara y después `Drop` volvía a detenerla; una parada/callback viejo podía cerrar una captura nueva | Una sola parada por Drop; JNI y callbacks comprueban el slot propietario. |
| P2 | Cancelar el futuro de arranque dejaba el sink/capturador registrado; excepciones durante initialize/start perdían recursos EGL/helper locales | Propietario Rust creado antes del primer await; recursos Kotlin asignados antes de initialize/start; limpieza también en `onDestroy`. Resultados de consentimiento cancelados se descartan. |
| P2 | Responder al consentimiento después de salir/cambiar de llamada podía activar compartir en otro estado | `open_screen_picker` conserva canal y época; vuelve a comprobar ambos antes de anunciar/publicar. |
| P2 | Logo y globo mantenían el compositor activo sin interacción | Logo estático y globo sin giro/pulso decorativo en móvil. Conserva arrastre, inercia, enfoque y actualización de pines; desktop conserva animación. |
| P2 | El empaquetador podía quedarse esperando EOF de pipes heredados por los daemons Gradle | Primera pasada escribe a archivo temporal; se conserva el diagnóstico de fallo. Servicio Kotlin copiado antes de la segunda pasada. Build completo terminó y validó DEX/JNI/marca. |

## Teléfono físico

Moto g15, Android 15/API 35, WebView 432×895 CSS px, APK **debug**. Dos builds del mismo checkout, mismo dispositivo, sin llamada ni cámara. Se preservaron identidad, ajustes y datos; no se enviaron mensajes. Inicio desconectado con directorio/globo visible. Estado térmico actual 0; los valores históricos de thermalservice no se interpretan como temperatura actual.

| Medida | Antes | APK corregido |
|---|---:|---:|
| CPU del proceso principal, mediana en Inicio, 20 s | 153.3% | 10.0% |
| CPU tras repetir Ajustes, mediana en reposo, 15 s | Sin muestra equivalente | 18.7% |
| PSS del proceso, mediana en Inicio | 300.7 MiB | 278.1 MiB; 302.4 MiB tras Ajustes |
| Abrir Ajustes, mediana de 10 ciclos | 85.8 ms | 83.0 ms |
| Cerrar Ajustes, mediana de 10 ciclos | 105.1 ms | 58.0 ms |

- **100% de CPU equivale a un núcleo**, no al teléfono completo. Se leen utime+stime del proceso; no incluye el renderer aislado de WebView. PSS es de `dumpsys meminfo` del paquete, sin sumar memoria compartida de otros procesos.
- Intervenciones temporales sobre el build anterior: solo globo pausado 134.9%; solo logo estático 145.1%; ambos pausados 18.5%. Cada uno por separado mantiene trabajo continuo del compositor. La mejora instalada concuerda con esta prueba, sin demostrar mejoras equivalentes en todas las pantallas.
- Ajustes se abre/cierra con clic sintético; latencia hasta cambio DOM + dos animation frames, **no** latencia táctil física ni final de la animación. Máximo de apertura corregida 152.8 ms. p95 de intervalos rAF durante la secuencia: 33.4 ms; no representa FPS de contenido pintado.
- Segunda secuencia de 10 ciclos: PSS mediana 359.5 MiB frente a 362.3 MiB en la primera; vuelve a 302.4 MiB en reposo. No hay crecimiento monotónico en esta muestra; no prueba ausencia de fugas a largo plazo.
- `gfxinfo` cuenta frames de la ventana, no todos los frames de contenido WebView. Al detener animación cambia el denominador: **no comparar porcentajes de jank** entre Inicio animado y estático. Cero long tasks JS tampoco descarta bloqueos en Rust/JNI.
- Estas muestras de reposo no establecen rendimiento release, batería, voz/video o red. Arranque y navegación se midieron por separado abajo.

## Navegación, cabecera y arranque

| Medida debug en el mismo Moto g15 | Antes | Corregido |
|---|---:|---:|
| Canales → Chat, mediana de 5 ciclos en #general ya cargado | 836.9 ms | 121.2 ms |
| Chat → Canales, mediana de 5 ciclos | 400.8 ms | 115.5 ms |
| Altura de cabecera móvil | 173.6 px con voz | 45.6 px, una fila sin voz |
| Proceso frío → interfaz visible, mediana de 3 | 4464 ms | 2805 ms; repetición con barras del sistema: 3277 ms |
| Reabrir proceso vivo → interfaz, mediana de 3 | 326 ms | 366 ms; sin mejora demostrada |

`MobileWorkspace` conserva Chat y Canales montados; `visibility:hidden` mantiene las dimensiones del virtualizador y `inert` evita interacción con el panel oculto. Se conserva el mismo nodo del chat y puente de scroll en todos los ciclos. Miembros sigue bajo demanda y los visores de pantalla conservan su ciclo de vida. Prueba de VirtualDom verifica un montaje, borrador retenido, atributos y limpieza.

Cabecera de una fila con logo, servidor/ruta y herramientas; controles táctiles de 44 px. Cifrado compacto en debug; release ya lo oculta sin desactivar E2EE. Guardados se conserva en Chat y se retira de la cabecera del workspace. **Hora, batería y barras del sistema visibles**, por preferencia del usuario; se retiró el experimento inmersivo que dejaba un área negra del recorte.

La identidad activa abre directamente su archivo canónico, descifra y compara la clave pública derivada antes de aceptarla; mantiene el escaneo de recuperación. La migración filtra versiones en claro antes de descifrar cuentas ya cifradas. No cambia el KDF, formato, Keystore ni recuperación. Las 16 pruebas de almacenamiento incluyen una sola derivación para la cuenta activa y rechazo de nombres/metadatos suplantados.

Arranque frío significa proceso detenido, **no cachés del dispositivo vacías**. El tiempo visible incluye conexión al inspector y espera del DOM; no mide reconexión al servidor. Tres muestras por lote y APK debug; variabilidad de 2.8–3.3 s después del cambio. No extrapolar a release.

## Medios en segundo plano

| Elemento | Implementación |
|---|---|
| Llamada | `BackgroundCall` pertenece a `ActiveVoice`: espera confirmación del servicio antes de conectar/capturar y se limpia también por error/cancelación. Slots impiden que una parada vieja afecte a otra llamada. |
| Cámara | Añade tipo `camera` y espera confirmación antes de abrir Camera2. Parar la cámara conserva la llamada/pantalla. |
| Pantalla | Consentimiento → tipo `mediaProjection` confirmado → capturador. Parar pantalla conserva llamada/cámara; fallo de consentimiento no detiene otra actividad. |
| Servicio | Solo los tipos activos `microphone|camera|mediaProjection`; permisos específicos en manifest. Notificación de medios y acceso a la app. Una adición rechazada no detiene el servicio de una llamada ya activa. No reinicia medios tras un cierre forzado (`START_NOT_STICKY`). |
| Pantalla apagada | Partial wake lock mientras exista servicio, renovación cada 5 min con timeout de 10 min y liberación en `onDestroy`; no enciende la pantalla. |

El inicio debe ocurrir desde la app visible por las [restricciones de permisos de Android](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start). Sin garantía frente a cierre forzado, presión extrema de memoria o restricciones del fabricante; el sistema también puede revocar MediaProjection. No se cambió la configuración de batería del teléfono.

Prueba física con APK final instalado a las 10:40:18: el usuario **confirma voz audible y recepción de cámara y pantalla compartida en la PC al cambiar de app**. ADB muestra mismo PID 13902, servicio `isForeground=true`, tipo microphone `0x80`, wake lock propio y estado `Dozing` con pantalla bloqueada durante la prueba de voz. Sin excepciones de servicio/captura en la muestra del proceso. `RUN_ANY_IN_BACKGROUND: allow`, standby bucket 10; esto no descarta otras políticas del fabricante. No se equipara la prueba corta a una llamada de horas ni valida video con pantalla bloqueada.

Observación de ~2 min (`target/mobile-tools/background-monitor.json`): 24/24 muestras con mismo PID, servicio foreground y wake lock; 10 en Dozing. Después se observó `types=0xA0` (microphone + mediaProjection); la recepción de video la confirmó el usuario desde la PC.

## Visor, audio y cámara — segunda revisión

| Fallo | Corrección / verificación |
|---|---|
| Visor inline reducido a una línea antes de conectar | Espacio móvil 16:9 reservado desde el montaje y estado de conexión hasta el primer frame presentado. Evento de un video reemplazado no oculta la carga del nuevo; regresión del bridge aprobada. Medición física: 396.8×223.2 px, sin overflow. |
| Agrandar/achicar y mute difíciles de tocar | Iconos SVG de 20 px y objetivos móviles de 44 px; volumen de 136×44 px. Se oculta ventana externa en móvil. |
| Volumen/mute esperando operaciones de voz/video | `StreamPlaybackControls` comparte los gains del mezclador y los actualiza desde el efecto de UI sin esperar la cola de conexiones/capturas. Prueba verifica ganancia, silencio y limpieza sin ejecutar el bucle de comandos. El usuario confirma mejor respuesta. |
| Selector de cámara negro | Android omitía la vista previa y marcaba `ready=true`. Ahora comparte el controlador nativo de Windows: cola de un frame, JPEG fuera del hilo de UI, rotación y liberación confirmada antes de publicar. Android limita la imagen local a 480×270 y 5 FPS; conserva resolución/FPS elegidos para la publicación. Imagen física comprobada y usuario confirma que se ve; no se guardaron imágenes personales. |

En 1440p/60 se observaron intervalos de 58–61 FPS sin nuevos descartes/congelamientos y otro de ~2 s con 50 FPS, 94 frames decodificados y 26 descartados; `packetsLost` no aumentó en ese intervalo. El aviso tiene evidencia de presión intermitente de reproducción; no se ocultó ni se atribuye toda interrupción al teléfono. `totalDecodeTime` no se interpreta solo como FPS máximos: puede incluir trabajo paralelo. Comparación 1080p/30 en curso. No se cambió el codec ni se redujo automáticamente la calidad de otras personas.

Pruebas: 9 relacionadas con preview aprobadas (incluye color, cola acotada, cierre y rotación); prueba del mezclador aprobada; bridge de pantallas y 19 casos de calidad aprobados; Clippy `-D warnings` aprobado. APK instalado y verificado DEX/JNI. Las muestras de decodificación y geometría están en `target/mobile-tools/stream-*`.

## Repetir

Node 22+ y el SDK de `scripts/android-env.ps1`. Abrir Discordia en Inicio, sin llamada, desbloqueada y visible. La medición no inicia sesiones ni activa medios.

```powershell
. ./scripts/android-env.ps1
node scripts/measure-android.mjs --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --seconds 20 --label home-idle
node scripts/measure-android.mjs --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --seconds 15 --label settings --scenario settings
node scripts/measure-android.mjs --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --seconds 15 --label tabs --scenario navigation
node scripts/measure-android-startup.mjs --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --kind cold --runs 3 --label startup-cold
node scripts/measure-android-startup.mjs --adb "$env:ANDROID_HOME/platform-tools/adb.exe" --kind warm --runs 3 --label startup-warm
```

`--scenario settings` abre/cierra diez veces y restaura el estado inicial del diálogo. Captura pasiva por defecto. Escribe JSON en `target/android-perf/<fecha>-<label>/report.json`; no recopila mensajes, imágenes ni texto introducido. Inspector USB temporal con puerto propio, retirado al finalizar. CPU requiere build debuggable/run-as; sin acceso queda como no disponible. Las muestras con error son incompletas, no baselines.

`navigation` requiere servidor/canal cargado, alterna cinco veces Chat/Canales y restaura la pestaña. Comparar el mismo historial caliente. `startup` detiene el proceso en frío o abre Inicio de Android en caliente: ejecutar **sin llamada/capturas**; no borra datos.

Artefactos de esta ejecución: `target/mobile-tools/review-phone-*.log`, `review-phone-settings-latency.json`, `review-ui-benchmark.log`, `android-review-final-build.log`, `android-review-consent-build.log` y `target/android-perf/`. Son locales, no versionados.

## Validación

| Comprobación | Resultado |
|---|---|
| `cargo check --workspace` | Aprobado, 14.04 s; warnings de caché incremental y bundle SFU |
| Check Android con `LIVEKIT_BUNDLE_SKIP=1`, sin defaults, feature mobile | Aprobado; 37 warnings previos |
| `cargo clippy -p discordia --lib -- -D warnings` | Aprobado con `LIVEKIT_BUNDLE_SKIP=1`; repetido fuera del sandbox por fallo inicial del archivador MSVC |
| `sysvideo::` / `e2ee::` | 14 aprobadas + 3 ignoradas / 7 aprobadas |
| `screenshare_bridge.cjs` | Aprobado, incluye ciclo de vida y guardas de cifrado WebView |
| `scripts/test-globe.mjs` | Reposo móvil/reduced-motion, foco, arrastre, suspensión y limpieza aprobados; agregado a CI |
| `scripts/test-measure-android.mjs` | Parsers de CPU/memoria/frames, counters ausentes/reiniciados aprobados; agregado a CI |
| Empaquetado / instalación | DEX/JNI/marca aprobados; `adb install -r` Success; proceso sobrevive las mediciones sin fallo nativo observado |

APK con servicio unificado instalado a las 10:40:18 locales, datos conservados. Kotlin, manifest con los tres tipos/permisos, DEX/JNI, Clippy con `-D warnings`, formato y regresiones JS aprobados. Las mediciones de navegación/arranque preceden ese servicio (que solo arranca al activar medios), no son un benchmark de voz/video.

Benchmark Windows debug existente (`benchmark_selective_ui`): 200 miembros, 200 cambios, siete muestras; voz ~7–8 ms/cambio de Virtual DOM. Excluye WebView/pintado y no sustituye medición Android. La compilación y pruebas JS no validan físicamente cámara/MediaProjection ni las carreras Kotlin; esa cobertura sigue dentro de las fases A/E del [plan](MOBILE_PLAN.md).
