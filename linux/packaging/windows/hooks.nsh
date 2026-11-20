; Установщик csqtt для Windows: служба csqttd ставится после копирования
; файлов и удаляется перед удалением программы.
!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Установка службы csqttd"
  nsExec::ExecToLog '"$INSTDIR\csqttd.exe" --install-service'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Удаление службы csqttd"
  nsExec::ExecToLog '"$INSTDIR\csqttd.exe" --uninstall-service'
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Обновление: старая служба держит файлы — останавливаем её.
  nsExec::ExecToLog 'sc.exe stop csqttd'
  Sleep 2000
!macroend
