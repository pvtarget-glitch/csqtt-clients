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
  ; Обновление: старая служба и окно держат файлы — останавливаем их.
  ; Туннель при этом отключается, служба после установки запускается снова.
  nsExec::ExecToLog 'sc.exe stop csqttd'
  nsExec::ExecToLog 'taskkill.exe /F /IM csqtt-gui.exe'
  Sleep 2000
!macroend
