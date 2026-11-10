# Discordia móvil (Android e iOS)

Un solo cliente Dioxus **0.7.10** (Cargo.lock) para escritorio y móvil: misma UI, estado y protocolo; lo propio de cada sistema queda tras `#[cfg(target_os = …)]` y el alias `crate::platform` (`dioxus::desktop`/`dioxus::mobile`, `client/src/lib.rs`). La app móvil es solo cliente. Android ARM64 se publica en cada pre-release; iOS nunca se compiló. Rutas de código Android: `docs/MAP.md`. Firma, publicación y actualizador: `docs/OPS.md`. Trabajo pendiente: GitHub issues.

## Estado por plataforma

✅ funciona · ◐ parcial · ❌ falta · — no aplica

| Área | Android | iOS | Archivo clave |
|---|---|---|---|
| Build y CI | ✅ APK ARM64 firmado en CI | ❌ sin compilar; `[ios]` de `Dioxus.toml` sin probar | `scripts/build-android.py`, `client/Dioxus.toml` |
| Identidad y bóveda | ✅ Keystore + `no_backup/discordia` | ◐ `keyring` apple-native sin verificar; `config_dir` cae en `dirs`/`temp_dir` | `android.rs`, `keyvault.rs`, `identity.rs` |
| Shell (pestañas, swipe, safe areas, teclado) | ✅ | ◐ CSS compartido, sin probar | `mobile_workspace.rs`, `assets/mobile.css` |
| Botón Atrás | ✅ predictivo y clásico | — | `mobile_back.rs`, `MainActivity.kt` |
| Ajustes | ✅ | ◐ sin probar | `settings_mobile.rs` |
| Idioma del sistema | ✅ | ❌ lee variables de entorno | `i18n.rs` |
| Archivos | ✅ SAF, copia privada acotada | ❌ devuelve error | `file_dialog.rs` |
| Portapapeles | ✅ texto e imágenes `content://` | ❌ devuelve error | `clipboard.rs` |
| Social / DMs | ✅ vista dentro de Inicio | ❌ abriría segunda ventana | `social_window.rs` |
| Voz | ◐ captura/reproducción; sin foco de audio, auriculares ni Bluetooth | ❌ sin sesión de audio | `audio_device.rs`, `features/voice.rs` |
| Cámara (envío) | ✅ Camera2 nativo | ❌ `syscamera::supported` falso | `syscamera.rs` |
| Pantalla (envío) | ✅ MediaProjection ≤1080p30, audio API 29+ | ❌ | `sysvideo/android.rs`, `sysaudio/android.rs` |
| Streams (recepción) | ✅ visor inline | ◐ sin probar | `features/screenshare.rs` |
| Segundo plano | ✅ servicio `microphone\|camera\|mediaProjection` + wake lock | ❌ | `DioxusForegroundService.kt` |
| Red y reanudación | ✅ callbacks a iroh y relays | ❌ | `quic/network_monitor.rs`, `nostr/src/relay.rs` |
| Notificaciones DM y menciones | ◐ FCM con el proceso muerto; sin probar en un teléfono. Sin push en builds sin `FIREBASE_*` ni en teléfonos sin Play services | ❌ | `dm_notifications.rs`, `push/`, `DioxusPushService.kt`, `DioxusNotifier.kt` |
| Controles de llamada en notificación | ✅ | ❌ | `call_notifications.rs` |
| Actualizador | ✅ PackageInstaller | — tienda | `update_android.rs` |
| Actividades | ❌ deshabilitadas | ❌ | `activity_window.rs` |
| Auto-host, juegos, bandeja, instancia única | — escritorio | — escritorio | `host.rs`, `presence/` |

## Decisiones y restricciones vigentes

| Decisión | Por qué |
|---|---|
| `MainActivity` en `singleTask`; el empaquetador lo verifica | Tao/`ndk-context` tienen un contexto nativo por proceso: una segunda Activity aborta con SIGABRT. |
| WebRTC se inicializa con `MainActivity.initializeWebRtc` en el hilo UI | Desde el looper JNI el classloader de arranque no ve las clases de WebRTC (`NoClassDefFoundError`). |
| Símbolos JNI de `webrtc-sys` exportados vía metadata `links` + `client/build.rs` | Los argumentos de enlace del build script de una dependencia no llegan al `.so` final. |
| `build-android.py` hace dos pasadas de `dx` e instala `webrtc.dx.init.gradle` en `$GRADLE_USER_HOME/init.d/` | `dx` corre Gradle antes de que exista `libwebrtc.jar` y no acepta `--init-script`. |
| Bóveda: HMAC no exportable de AndroidKeyStore → passphrase NIP-49; nunca el `keyring` mock | El mock no persiste. Alias y etiqueta (`android.rs`) son formato persistente: cambiarlos exige migración. |
| Identidad en `no_backup/` | Una copia restaurada no puede descifrarse sin la clave Keystore del equipo original. |
| Envío de cámara y pantalla nativo por JNI, no WebView | Android WebView no tiene `getDisplayMedia`; la cámara no admite dos aperturas. Ver y la preview siguen en el WebView/superficie. |
| Orden: consentimiento → servicio `mediaProjection` confirmado → capturador | Android 14 exige el FGS antes del display virtual; el inicio debe hacerse con la app visible. |
| Servicio `START_NOT_STICKY` | Tras un cierre forzado no se reinicia captura sin nuevo consentimiento. |
| El observador Kotlin no libera el `VideoFrame` | Lo libera el capturador; liberarlo dos veces mata el hilo de cámara. |
| Copia I420 con `stride*(rows-1)+row_len` | Los planos U/V de `YuvConverter` comparten fila: la última es media fila. |
| Primer frame señalizado sin mirar `capture_frame` | Antes de publicar no hay consumidor y devuelve `false`; esperar `true` agotaba el timeout. |
| Sin camino zero-copy | La rotación fuerza `ToI420()` igual y el modo surface exigiría compartir el contexto EGL con el encoder. |
| Presupuesto de captura limita lo **capturado** (`mobile_budget.rs`), con `FramePacer` | El coste sigue al readback de GPU, no a la resolución codificada; el display virtual entrega a la tasa de pantalla. |
| `window_active`/`window_shown` devuelven `true` en móvil | Tao responde `false` a `is_focused`/`is_visible` en Android. |
| Ruta virtual `Default Device` conservada por `audio_device::missing` | CPAL/AAudio la devuelve como predeterminada sin enumerarla; sin esto el micrófono se reabría cada escaneo. |
| Audio de pantalla reutiliza el MediaProjection y excluye el UID propio | Un segundo token pediría otro consentimiento; sin la exclusión la llamada vuelve en eco. |
| Gateway y servicios de medios avanzan en Tokio, no al ritmo de los ACK del WebView | Android suspende el WebView en segundo plano; el heartbeat no puede depender del render. |
| Cambios de red reenviados a iroh, incluido `onBlockedStatusChanged(false)` | El monitor de iroh no los recibe solo en Android, y la red puede desbloquearse después de `onResume`. |
| `network_security_config.xml` permite cleartext | La sala WebView debe alcanzar un SFU `ws://` fuera de loopback. |
| Permisos de micrófono y cámara se piden al usar, no al entrar | Un rechazo no debe impedir chatear. |
| Shell móvil por plataforma, no por breakpoint | Girar a horizontal superaba `768px` y volvía el layout de escritorio. |
| Actividades deshabilitadas en móvil | Trampa 35: no pueden correr en el WebView principal y no hay ventana aislada móvil. |
| Barras del sistema visibles | Preferencia del usuario; el modo inmersivo dejaba un área negra en el recorte. |
| `discordia-server` sigue enlazado; `LIVEKIT_BUNDLE_SKIP=1` solo omite el SFU | El cliente usa tipos y helpers del servidor (`net.rs`, `quic.rs`, `voice.rs`). |
| El servicio FCM recibe directorio, idioma y `JNIEnv` como argumentos | Un push puede arrancar el proceso sin Activity: `ndk_context` no está inicializado y `android::config_dir()` termina el proceso. Inicializarlo desde el servicio haría abortar a Tao al crear la Activity. |
| El push nunca crea la clave del Keystore | Una clave nueva dejaría ilegibles todas las identidades guardadas (trampa 23); sin clave, no hay aviso. |
| Una sola `DioxusNotifier.showDm` para el camino vivo y el push, con el último `at` avisado por etiqueta | Un DM puede llegar por los dos caminos; así avisa una vez. |
| Nombres de canal guardados en `SharedPreferences`, inglés si faltan | En un proceso frío no hay Rust de UI que los traduzca. |
| Código bajo `cfg(target_os = "android")` no pasa por Clippy en CI | Trampa 14 cubre solo targets de escritorio; el build Android tolera warnings. |
| Nunca desinstalar para probar | Borra la identidad Keystore; usar `adb install -r`. Debug y Release coexisten (`docs/OPS.md`). |

## Compilar y medir en el dispositivo

Release y firma: `docs/OPS.md`. Requiere SDK 34, NDK 27.2.12479018, Java 21 y `dx` 0.7.10.

| Comando | Para qué |
|---|---|
| `python scripts/setup-android.py --accept-sdk-license` | Descarga SDK/NDK/JDK verificados en `target/mobile-tools` (Windows). |
| `. ./scripts/android-env.ps1` | Variables solo para la terminal actual. |
| `python scripts/build-android.py --install` | APK Debug (`com.discordia.app.debug`) + `adb install -r`; valida DEX/JNI/marca. |
| `python scripts/build-android.py --gateway-diagnostics` | APK con trazas USB del transporte, sin payloads. |
| `node scripts/measure-android.mjs --seconds 20 --label home-idle` | CPU/PSS/frames en reposo; `--scenario settings\|navigation`. |
| `node scripts/measure-android-startup.mjs --kind cold\|warm --runs 3` | Arranque frío/caliente, sin llamada activa. |
| `python scripts/test-android-capture.py` / `test-android-network.py` | Kotlin real de captura y red en JVM (CI, tras el build). |

Mediciones: añadir `--package com.discordia.app.debug` para Debug; necesitan build debuggable; 100% de CPU = un núcleo, sin el renderer aislado del WebView; `navigation` necesita un canal cargado. Salida en `target/android-perf/<fecha>-<label>/report.json`. Una medición Debug no vale como Release.
