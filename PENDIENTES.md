# Pendientes del fork

Lista de trabajo de este fork (`softjuarez/coucou`, rama `claude-code-chat`). Solo aplica a la app de Windows (`windows/`).

## Requerimientos nuevos

### 1. Traducir la app a español
Todos los textos visibles: vistas de la isla, tarjeta de archivo (dibujada en canvas), ventana de Settings, menú de la bandeja y mensajes de error que vienen de Rust.

- Hoy no existe sistema de traducción: los textos están escritos en inglés dentro del código (y quedan algunos en francés heredados del Mac).
- Decisión abierta: reemplazar los textos por español directamente, o agregar un diccionario español/inglés con selector en Settings.
- Los botones dibujados en canvas tienen ancho fijo y hay que ajustarlos al texto en español.

### 2. Hablarle por micrófono
Dictar al chat en lugar de escribir.

- Paso cero, sin programar: probar el dictado de Windows (`Win + H`) con el cursor en el campo del chat.
- Opciones para un botón de micrófono propio: reconocimiento de voz de Windows (nativo, sin costo), Whisper local (mejor calidad, descarga de modelo) o un servicio en la nube (requiere clave y envía el audio fuera).
- Requiere permiso de micrófono en el WebView y un indicador claro de que está escuchando.

### 3. Abrir aplicaciones y hacer búsquedas en el navegador
Pedirle a Mochi "abre Excel" o "busca X en el navegador".

- Hoy el chat no puede ejecutar nada en el equipo, a propósito.
- Propuesta: herramientas propias y acotadas (`abrir_aplicacion`, `abrir_url`, `buscar_en_navegador`) en lugar de darle una terminal.
- Abrir solo aplicaciones instaladas, por nombre y sin argumentos; URLs solo http/https.
- Decisión abierta: "búsquedas de navegación" como abrir el navegador con los resultados, o como que el agente navegue y opere páginas por su cuenta (mucho más trabajo y riesgo).

## Pendientes anteriores

- **Workflow de n8n**: crear la credencial del secreto del webhook, publicarlo y probarlo con el número de pruebas.
- **Adjuntar el archivo en Chatwoot**: hoy solo se registra un mensaje de texto con el nombre del archivo.
- **"Permitir siempre"** en la tarjeta de aprobación del chat, por herramienta.
- **Editar un destino de "Send to"** sin tener que quitarlo y volver a agregarlo.
- **Varios archivos y carpetas** al soltar: hoy solo toma el primero.
- **Formatos antiguos** `.doc`, `.xls`, `.ppt`: no se leen.
- **Selector de modelo** para el chat con Claude Code: hoy usa el modelo por defecto del plan.
- **Grabaciones y transcripciones de Teams**: verificar si se alcanzan por el conector de Microsoft 365 o hace falta n8n con Microsoft Graph.
- **WhatsApp personal**: sin API oficial; descartado dentro de la app.

## Antes de compartirlo en la oficina

- **Marca**: el nombre Coucou, Mochi, el icono y los sonidos están reservados por el autor. Pedir permiso o cambiar la marca.
- **Instalador firmado**: sin firma, Microsoft Defender lo marca como malware.
- **Requisito por persona**: Claude Code instalado y con sesión iniciada, o una clave de API.

## Mantenimiento

- Rebasar contra el upstream con frecuencia (avanza varias versiones por día).
- El PR #114 del upstream (arrastre de archivos) está integrado aquí; cuando lo fusionen allá, quitar la copia local.
- Compilar desde el entorno de las Build Tools 2022 (`vcvars64.bat`); la instalación de Visual Studio 2022 Community tiene el C++ incompleto.
