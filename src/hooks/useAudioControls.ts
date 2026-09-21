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
    setGainError('')
    if (platform === 'windows') {
      setAudioGainReady(false)
      void invoke<number>('get_audio_gain').then((value) => {
        if (!active) return
        const next = Math.max(audioGainMin, Math.min(audioGainMax, Math.round(value)))
        setAudioGain(next)
        setAudioGainReady(true)
      }).catch((error) => {
        if (!active) return
        logError('Failed to read Windows service audio gain', error)
        setGainError(`音频增益读取失败：${String(error)}`)
        setAudioGainReady(false)
      })
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
    return () => { active = false }
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
