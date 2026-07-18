# Сайдкар инференса TaleShift

[English](README.md) | Русский

`serve.py` — это локальный процесс инференса, который использует приложение на Rust. Он может
размещать пять необязательных компонентов в одном API:

- эмбеддинги Qwen3 (`POST /v1/embeddings`);
- реранкинг Jina (`POST /rerank`);
- мультиязычное распознавание речи Whisper Small (`POST /transcribe` с необработанным аудио WebM/WAV);
- синтез речи Qwen3 (`POST /speak`, `POST /speak_stream`);
- генерацию изображений ComfyUI/FLUX.2 (`POST /images/generate`).

Приложение запускает этот процесс по мере необходимости и ожидает `GET /health`.
PyAV декодирует аудио из браузера с помощью библиотек, включённых в wheel-пакет, поэтому для
локального распознавания речи не требуется системный исполняемый файл ffmpeg. Когда локальное
распознавание речи недоступно или отключено, сервер на Rust продолжает использовать существующий
резервный механизм транскрибации через коннектор.

## Установка

Не собирайте окружения Python и каталоги моделей вручную. В корне репозитория
выполните:

```powershell
.\setup.cmd -Profile Rag
```

Доступны накопительные профили `Minimal`, `Rag`, `Voice`, `Images` и
`Full`. Скрипт установки создаёт воспроизводимые окружения из lock-файлов,
скачивает неизменяемые ревизии моделей из `models.json`, проверяет значения SHA-256
и записывает маркеры готовности только после полной установки каждого компонента.

Структура по умолчанию:

```text
%LOCALAPPDATA%\gm-lab\inference\
  runtime\.venv\
  models\embedder\
  models\reranker\
  models\stt\
  tts\qwen17b_base\
  image\.venv\
  image\ComfyUI\
  logs\sidecar.log
```

Прежнее имя каталога `gm-lab` намеренно сохранено для совместимости
с существующими установками.

Используйте `setup.cmd -InferenceHome <path>`, чтобы разместить эту структуру в другом месте.
При повторном запуске установки скачивание моделей продолжится с места остановки. Учётные данные
Hugging Face необязательны для текущих публичных артефактов и никогда не сохраняются скриптом установки.

## Важные замечания о совместимости

- Для текстовых моделей и синтеза речи требуется NVIDIA Ampere/RTX 30 или новее с поддержкой BF16.
- Профиль изображений использует NVFP4 и поддерживается на NVIDIA Blackwell / RTX 50
  (compute capability 10 или выше).
- Flash Attention необязателен. `USE_FLASH=auto` включает его только при наличии
  импортируемого совместимого пакета.
- Jina Reranker распространяется по лицензии CC BY-NC 4.0. Метаданные лицензий энкодера изображений и VAE
  сейчас неполны. Ознакомьтесь с [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md).

## Автономная разработка

После установки активируйте управляемое окружение или вызовите его Python напрямую:

```powershell
$root = "$env:LOCALAPPDATA\gm-lab\inference"
$env:GM_INFERENCE_HOME = $root
$env:EMBEDDER_ENABLED = "1"
$env:RERANKER_ENABLED = "1"
$env:STT_ENABLED = "1"
$env:EMBEDDER_MODEL = "$root\models\embedder"
$env:RERANKER_MODEL = "$root\models\reranker"
$env:STT_MODEL = "$root\models\stt"
$env:TTS_ENABLED = "0"
$env:IMAGE_ENABLED = "0"
& "$root\runtime\.venv\Scripts\python.exe" .\sidecar\serve.py
```

Адрес API по умолчанию — `http://127.0.0.1:8077`. Логи записываются в
`<inference-home>/logs/sidecar.log`.

Основные переменные окружения:

| Переменная | Назначение |
|---|---|
| `GM_INFERENCE_HOME` | Корень управляемой установки |
| `EMBEDDER_ENABLED`, `RERANKER_ENABLED` | Включение компонентов RAG |
| `STT_ENABLED`, `TTS_ENABLED`, `IMAGE_ENABLED` | Включение необязательных мультимедийных компонентов |
| `EMBEDDER_MODEL`, `RERANKER_MODEL`, `STT_MODEL` | Каталог предварительно установленного локального снимка |
| `EMBEDDER_QUANT`, `RERANKER_QUANT` | `bf16` или `nf4` |
| `STT_MAX_BYTES`, `STT_MAX_SECONDS` | Ограничения размера необработанного тела и длительности декодированного аудио (по умолчанию 32 МиБ / 600 с) |
| `USE_FLASH` | `auto`, `1` или `0` |
| `GMLAB_SIDECAR_PORT` | Порт API, по умолчанию `8077` |
| `GMLAB_ALLOW_RUNTIME_DOWNLOADS` | Разрешение ситуативного скачивания эмбеддера и реранкера; по умолчанию отключено. STT всегда загружается только локально |
| `IMAGE_COMFY_PORT` | Порт ComfyUI, по умолчанию `8188` |

Проверка целостности без скачивания:

```powershell
.\setup.cmd -Profile Rag -VerifyOnly
```
