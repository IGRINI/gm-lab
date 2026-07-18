# Third-party models and runtimes

TaleShift source code is licensed under Apache-2.0. Models, voice references and external
runtimes are separate works and keep their own licenses. `setup.ps1` downloads
them directly from the sources below; they are not relicensed by TaleShift.

| Component | Immutable source | Declared license |
|---|---|---|
| Qwen3 Embedding 0.6B | `Qwen/Qwen3-Embedding-0.6B@97b0c614be4d77ee51c0cef4e5f07c00f9eb65b3` | Apache-2.0 |
| Jina Reranker v3 | `jinaai/jina-reranker-v3@10fb694fc21f7a710a563ff1eb977a460f3868e4` | CC BY-NC 4.0 |
| OpenAI Whisper Small | `openai/whisper-small@973afd24965f72e36ca33b3055d56a652f456b4d` | Apache-2.0 |
| Qwen3 TTS 1.7B Base | `Qwen/Qwen3-TTS-12Hz-1.7B-Base@fd4b254389122332181a7c3db7f27e918eec64e3` | Apache-2.0 |
| faster-qwen3-tts and reference WAV files | `andimarafioti/faster-qwen3-tts@7cdef7e40195108b51a808f2ce5c7d5f3e235a79` | MIT repository |
| FLUX.2 Klein 4B NVFP4 | `black-forest-labs/FLUX.2-klein-4b-nvfp4@1db2b2f776c24b76f1122e5f69ab1949fc620068` | Apache-2.0 |
| FLUX.2 FP4 text encoder and VAE files | `Comfy-Org/vae-text-encorder-for-flux-klein-4b@a9e4ca87c16db4c4e1a16406a9ddb300ab0ae246` | No license declared by the repository |
| ComfyUI | `comfyanonymous/ComfyUI@1a510f04234e5a213d3985a1a54f65652623f4bc` | GPL-3.0 |
| Hugging Face Diffusers | `huggingface/diffusers@bd2c91958881b777260eedb1c3d61d01c03e800f` | Apache-2.0 |
| PyAV 18.0.0 and bundled media libraries | Python runtime wheel | PyAV: BSD-3-Clause; bundled libraries retain their upstream licenses |

The complete machine-readable list, file names, sizes and SHA-256 values is in
[`sidecar/models.json`](sidecar/models.json).

## Important restrictions

- `jina-reranker-v3` is licensed under CC BY-NC 4.0. The current `Rag`,
  `Voice`, `Images` and `Full` profiles therefore are not suitable for
  commercial use without separate permission or a replacement reranker.
- The Comfy-Org repository used for the exact FP4 text encoder and VAE does not
  declare a license. Accepting the setup warning does not create a license
  grant. Do not redistribute or use these files commercially until their terms
  are clarified.
- The three reference WAV files are tracked by the MIT-licensed
  faster-qwen3-tts repository, but separate performer consent or voice rights
  are not documented there. Review those rights before public or commercial
  use of cloned voices.
- ComfyUI runs as a separate process and is downloaded into the local inference
  directory. If you redistribute ComfyUI itself, comply with GPL-3.0.

This notice is informational and is not legal advice. Always review the
upstream model cards and license texts before redistribution.
