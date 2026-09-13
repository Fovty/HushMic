# Model comparison

## Method

The three demo clips (keyboard, fan, café) were built from one public-domain voice recording mixed with public noise recordings at about +3 dB SNR; the recipe and every source are in [demo/ASSETS.md](demo/ASSETS.md) and [demo/make-demos.py](demo/make-demos.py). Each clip was run through every model and the output scored with DNSMOS P.835, a reference-free estimator that predicts a 1 to 5 mean opinion score for overall quality (OVRL), background noise (BAK) and speech quality (SIG). Higher is better.

| Model                             | OVRL | BAK  | SIG  |
| --------------------------------- | :--: | :--: | :--: |
| **DPDFNet** (HushMic)             | 3.20 | 4.15 | 3.43 |
| DeepFilterNet 3                   | 2.97 | 3.98 | 3.26 |
| Krisp v9.9.3                      | 2.57 | 3.96 | 2.81 |
| khip (older Krisp model port)     | 2.43 | 3.78 | 2.71 |
| GTCRN                             | 2.42 | 3.71 | 2.76 |
| RNNoise (EasyEffects default)     | 2.01 | 3.93 | 2.60 |
| Unprocessed input                 | 1.49 | 1.48 | 2.01 |

Averages over the three clips. DPDFNet scores highest on all three axes and is the only model that keeps its score on the café clip, where the others drop.

## What the numbers do not say

Three constructed clips with one voice are not a listening test and do not predict every microphone, room or language. DNSMOS is an estimate. The scores are there to explain why HushMic uses DPDFNet, not to rank the other projects in general. The scoring run itself (model versions, settings, per-clip outputs) is not versioned in this repository yet; the source audio is, so the comparison can be repeated.

## Demo clip measurements

Measured on the July 2026 build in the speech pauses of each clip: keyboard noise drops about 25 to 27 dB, fan hum about 25 to 30 dB, and café chatter 21 to 37 dB, with the speech level within 2.5 dB of the input. The per-clip readings are in [demo/ASSETS.md](demo/ASSETS.md).

## CPU cost

The quality model (`dpdfnet8_48khz_hr`) runs at roughly 0.3 times real time on one desktop core; this is an informal measurement, not a guarantee for your machine. The light model (`dpdfnet2_48khz_hr`) is cheaper at some cost in suppression. Latency is the same for both: 80 ms, see [troubleshooting.md](troubleshooting.md#latency-and-cpu).

## About DPDFNet

DPDFNet is a DeepFilterNet-lineage speech-enhancement model by Ceva, released under Apache-2.0: [github.com/ceva-ip/DPDFNet](https://github.com/ceva-ip/DPDFNet), [arXiv:2512.16420](https://arxiv.org/abs/2512.16420).
