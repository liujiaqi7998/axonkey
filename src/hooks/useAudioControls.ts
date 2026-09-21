import { invoke } from '@tauri-apps/api/core'
import { useEffect, useState } from 'react'
import {
  audioGainMax,
  audioGainMin,
  audioSettingsStorageKey,
  getStoredAudioGain,
} from '../appConfig'
import type { Platform } from '../appTypes'
import { logError, logInfo } from '../runtimeLogging'

type UseAudioControlsOptions = {
  platform: Platform
  nativeRuntime: boolean
  onToast: (message: string) => void
}

function formatWindowsGainError(error: unknown) {
  const detail = String(error)
  return /os error 2|系统找不到指定的文件/.test(detail)
    ? 'AxonkeyService 未运行或 RPC 尚未就绪，请先在设置中启动 AxonkeyService。'
    : `音频增益读取失败：${detail}`
}

export function useAudioControls({ platform, nativeRuntime, onToast }: UseAudioControlsOptions) {
  const [audioGain, setAudioGain] = useState(() => platform === 'macos' ? getStoredAudioGain() : 0)
  const [gainError, setGainError] = useState('')
  const [audioGainReady, setAudioGainReady] = useState(() => platform !== 'windows' || !nativeRuntime)

  useEffect(() => {
    if (platform !== 'macos') return
    window.localStorage.setItem(audioSettingsStorageKey, JSON.stringify({ gain: audioGain }))
  }, [audioGain, platform])

  useEffect(() => {
    if (platform === 'unsupported' || !nativeRuntime) {
      setAudioGainReady(true)
      return
    }
    let active = true
    let retryTimer: number | undefined
    setGainError('')
    if (platform === 'windows') {
      setAudioGainReady(false)
      let hasLoggedFailure = false
      const readWindowsGain = async () => {
        if (!active) return
        try {
          const value = await invoke<number>('get_audio_gain')
          if (!active) return
          const next = Math.max(audioGainMin, Math.min(audioGainMax, Math.round(value)))
          setAudioGain(next)
          setGainError('')
          setAudioGainReady(true)
        } catch (error) {
          if (!active) return
          if (!hasLoggedFailure) {
            logError('Failed to read Windows service audio gain', error)
            hasLoggedFailure = true
          }
          setGainError(formatWindowsGainError(error))
          setAudioGainReady(false)
          retryTimer = window.setTimeout(() => void readWindowsGain(), 1500)
        }
      }
      void readWindowsGain()
    } else {
      setAudioGainReady(true)
      const storedGain = getStoredAudioGain()
      setAudioGain(storedGain)
      void invoke('set_audio_gain', { gain: storedGain }).catch((error) => {
        if (!active) return
        logError('Failed to initialize audio gain', error)
        setGainError(`音频增益未生效：${String(error)}`)
      })
    }
    return () => {
      active = false
      if (retryTimer !== undefined) window.clearTimeout(retryTimer)
    }
  }, [nativeRuntime, platform])

  const updateAudioGain = (value: number) => {
    const next = Math.max(audioGainMin, Math.min(audioGainMax, Math.round(value)))
    setAudioGain(next)
    setGainError('')
    if (platform === 'unsupported' || !nativeRuntime || !audioGainReady) return
    logInfo(`Updating audio gain from frontend: ${next} dB`)
    void invoke('set_audio_gain', { gain: next }).catch((error) => {
      logError('Failed to update audio gain', error)
      setGainError(`音频增益未生效：${String(error)}`)
      onToast(`音频增益未生效：${String(error)}`)
      window.setTimeout(() => onToast(''), 2600)
    })
  }

  return { audioGain, gainError, audioGainReady, updateAudioGain }
}
