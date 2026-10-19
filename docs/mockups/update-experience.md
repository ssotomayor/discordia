# Actualizaciones

![Boceto](update-experience.png)

| Elemento | Comportamiento |
|---|---|
| Aviso superior | Visible en Inicio y dentro de servidores. «Ver actualización» abre Configuración → Actualizaciones. Se puede ocultar; la oferta sigue accesible desde Configuración. |
| Buscar actualizaciones | Botón manual disponible sin desconectarse. Muestra el resultado y la última comprobación; evita solicitudes simultáneas. |
| Disponible | Acción principal «Descargar actualización» y acceso a novedades. |
| Descargando | Progreso real cuando se conoce el tamaño; permite seguir usando la app. |
| Lista para instalar | «Reiniciar y actualizar» es una acción explícita, separada de descargar. Se habilita al terminar la llamada y respeta el cierre ordenado de la sesión. |
| Sin novedades | «Estás al día», versión instalada y botón para volver a comprobar. |
| Error | Mensaje visible y acción para repetir la búsqueda o descarga. Un fallo al comprobar no borra una actualización ya encontrada. |

Implementado en `client/src/update_ui.rs`; el boceto es la referencia visual.
La verificación de firmas y la instalación siguen en `client/src/update.rs`.
