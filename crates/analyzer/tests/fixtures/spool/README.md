# Spool fixtures

Capturas reales del spooler para probar el análisis sin imprimir.

Formato esperado: el `.SPL` crudo que deja el spooler, más su `.SHD` si existe
(contiene el DEVMODE y el nombre de la cola). También sirven los `.EMF`, `.XPS`
y `.PDF` intermedios que produce el propio agente en
`%APPDATA%\mypaper-print-agent\`.

Cómo capturar uno:

1. Retén un trabajo con el agente (`mpa watch` o la ventana).
2. Copia `C:\Windows\System32\spool\PRINTERS\<job>.SPL` aquí **antes** de
   reanudar el trabajo (requiere consola elevada).
3. Copia también `%APPDATA%\mypaper-print-agent\agent-state.json` si quieres el
   contexto de la cola y el tamaño de hoja.

Reglas:

- Nada de datos del cliente: los tickets reales llevan nombres. Recorta o usa
  trabajos de prueba propios.
- Los archivos aquí son binarios; `.gitattributes` ya los marca como tales.
- Los tests deben tolerar la carpeta vacía: nunca asumir que hay fixtures.
