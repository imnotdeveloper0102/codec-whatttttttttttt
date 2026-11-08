// CodecWhat.mm — React Native module for codec-whatttttttttttt
// (Phase 10). Thin adapter over the C ABI: every method converts
// base64 / bridge values, calls the C functions, and converts the
// result back. No decode or encode logic lives here.
//
// Binary data crosses the bridge as base64 strings and audio
// samples as arrays of numbers; see the package README for the
// cost of that.
//
// This is a plain RCTBridgeModule (not the codegen spec): it
// works on both architectures through the TurboModule interop
// layer, and avoids requiring the app to run codegen before this
// module compiles. A later version should switch to the generated
// spec.
//
// UNVERIFIED: not built or run — no macOS/Xcode in this
// environment (see the package README).

#import "CodecWhat.h"

#import <React/RCTLog.h>

#include "codec_whatttttttttttt.h"

namespace {

// Reject a promise with a codec error: the code is the C ABI
// numeric code as a string, the message is the per-thread detail
// (falling back to the generic string for the code).
void rejectCodec(RCTPromiseRejectBlock reject, codec_error_t rc) {
  const char *detail = codec_last_error();
  if (detail == nullptr || detail[0] == '\0') {
    detail = codec_error_string(rc);
  }
  NSString *message = detail != nullptr
      ? [NSString stringWithUTF8String:detail]
      : @"codec error";
  reject([NSString stringWithFormat:@"%d", (int)rc], message, nil);
}

// Decode a base64 string to bytes; returns nil and rejects on bad input.
NSData *decodeBase64(NSString *text, RCTPromiseRejectBlock reject) {
  NSData *data = [[NSData alloc] initWithBase64EncodedString:text
                                                       options:0];
  if (data == nil) {
    reject(@"7", @"invalid base64 input", nil);
  }
  return data;
}

NSString *encodeBase64(const uint8_t *bytes, size_t len) {
  NSData *data = [NSData dataWithBytes:bytes length:len];
  return [data base64EncodedStringWithOptions:0];
}

// The PCM data chunk of a WAV file. The decoder needs the data
// chunk, not the RIFF header: walk the chunks and return the
// "data" payload (chunks are word-aligned).
// Returns NO on failure and sets `rc` to the codec error code.
BOOL wavDataChunk(const uint8_t *wav, size_t len, size_t *outOffset,
                  size_t *outLen, codec_error_t *rc) {
  if (len < 12) {
    *rc = CODEC_ERR_INVALID_DATA;
    return NO;
  }
  size_t offset = 12;
  while (offset + 8 <= len) {
    if (memcmp(wav + offset, "data", 4) == 0) {
      uint32_t size = (uint32_t)wav[offset + 4] |
                      ((uint32_t)wav[offset + 5] << 8) |
                      ((uint32_t)wav[offset + 6] << 16) |
                      ((uint32_t)wav[offset + 7] << 24);
      if (offset + 8 + size > len) {
        *rc = CODEC_ERR_TRUNCATED;
        return NO;
      }
      *outOffset = offset + 8;
      *outLen = size;
      return YES;
    }
    uint32_t size = (uint32_t)wav[offset + 4] |
                    ((uint32_t)wav[offset + 5] << 8) |
                    ((uint32_t)wav[offset + 6] << 16) |
                    ((uint32_t)wav[offset + 7] << 24);
    offset += 8 + size + (size & 1);
  }
  *rc = CODEC_ERR_INVALID_DATA;
  return NO;
}

NSDictionary *audioFrameToDictionary(const codec_audio_frame_t *frame) {
  NSMutableArray *samples = [NSMutableArray
      arrayWithCapacity:(NSUInteger)(frame->frames * frame->channels)];
  for (uint64_t i = 0; i < frame->frames * frame->channels; i++) {
    [samples addObject:@(frame->samples[i])];
  }
  return @{
    @"sampleRate" : @(frame->sample_rate),
    @"channels" : @(frame->channels),
    @"frames" : @(frame->frames),
    @"samples" : samples,
  };
}

} // namespace

@implementation CodecWhat

// Streaming encoders held by handle (small integers from 1).
// Guarded by @synchronized; the bridge calls methods serially,
// but the map is also touched from `invalidate`.
static NSMutableDictionary<NSNumber *, NSValue *> *encoders = nil;
static int32_t nextEncoderHandle = 1;

+ (void)initialize {
  if (self == [CodecWhat class]) {
    encoders = [NSMutableDictionary dictionary];
  }
}

RCT_EXPORT_MODULE(CodecWhat)

- (NSDictionary *)constantsToExport {
  return @{@"version" : [NSString stringWithUTF8String:codec_version()]};
}

RCT_EXPORT_METHOD(probe
                  : (NSString *)dataBase64 resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  NSData *data = decodeBase64(dataBase64, reject);
  if (data == nil) {
    return;
  }
  codec_probe_t result;
  codec_error_t rc =
      codec_probe((const uint8_t *)data.bytes, data.length, &result);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  resolve(@{@"container" : @(result.container), @"codec" : @(result.codec)});
}

RCT_EXPORT_METHOD(decodeWav
                  : (NSString *)dataBase64 resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  NSData *wav = decodeBase64(dataBase64, reject);
  if (wav == nil) {
    return;
  }
  const uint8_t *bytes = (const uint8_t *)wav.bytes;
  codec_decoder_t *decoder = nullptr;
  codec_error_t rc =
      codec_decoder_from_wav_header(bytes, wav.length, &decoder);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  size_t dataOffset = 0;
  size_t dataLen = 0;
  if (!wavDataChunk(bytes, wav.length, &dataOffset, &dataLen, &rc)) {
    codec_decoder_free(decoder);
    rejectCodec(reject, rc);
    return;
  }
  const codec_audio_frame_t *frame = nullptr;
  rc = codec_decoder_decode(decoder, bytes + dataOffset, dataLen, 0, &frame);
  if (rc != CODEC_OK) {
    codec_decoder_free(decoder);
    rejectCodec(reject, rc);
    return;
  }
  NSDictionary *result = audioFrameToDictionary(frame);
  codec_decoder_free(decoder);
  resolve(result);
}

RCT_EXPORT_METHOD(decodeFlac
                  : (NSString *)streaminfoBase64 packetBase64
                  : (NSString *)packetBase64 resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  NSData *streaminfo = decodeBase64(streaminfoBase64, reject);
  if (streaminfo == nil) {
    return;
  }
  NSData *packet = decodeBase64(packetBase64, reject);
  if (packet == nil) {
    return;
  }
  if (streaminfo.length != 34) {
    reject(@"7", @"streaminfo must be 34 bytes", nil);
    return;
  }
  codec_decoder_t *decoder = nullptr;
  codec_error_t rc = codec_decoder_from_flac_streaminfo(
      (const uint8_t *)streaminfo.bytes, 34, &decoder);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  const codec_audio_frame_t *frame = nullptr;
  rc = codec_decoder_decode(decoder, (const uint8_t *)packet.bytes,
                            packet.length, 0, &frame);
  if (rc != CODEC_OK) {
    codec_decoder_free(decoder);
    rejectCodec(reject, rc);
    return;
  }
  NSDictionary *result = audioFrameToDictionary(frame);
  codec_decoder_free(decoder);
  resolve(result);
}

RCT_EXPORT_METHOD(encodeFlac
                  : (NSArray<NSNumber *> *)samples frames
                  : (double)frames channels
                  : (double)channels sampleRate
                  : (double)sampleRate resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  uint64_t frameCount = (uint64_t)frames;
  uint16_t channelCount = (uint16_t)channels;
  if (samples.count != frameCount * channelCount) {
    reject(@"7", @"samples length does not match frames * channels", nil);
    return;
  }
  int16_t *pcm = (int16_t *)malloc(samples.count * sizeof(int16_t));
  if (pcm == nullptr) {
    reject(@"6", @"allocation failed", nil);
    return;
  }
  for (NSUInteger i = 0; i < samples.count; i++) {
    pcm[i] = (int16_t)[samples[i] intValue];
  }
  uint8_t *outData = nullptr;
  size_t outLen = 0;
  codec_error_t rc = codec_encode_flac(pcm, frameCount, channelCount,
                                       (uint32_t)sampleRate, &outData, &outLen);
  free(pcm);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  NSString *result = encodeBase64(outData, outLen);
  codec_free(outData);
  resolve(result);
}

RCT_EXPORT_METHOD(parseVp9FrameHeader
                  : (NSString *)frameBase64 resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  NSData *frame = decodeBase64(frameBase64, reject);
  if (frame == nil) {
    return;
  }
  codec_vp9_frame_header_t header;
  codec_error_t rc = codec_vp9_parse_frame_header(
      (const uint8_t *)frame.bytes, frame.length, &header);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  resolve(@{
    @"profile" : @(header.profile),
    @"frameType" : @(header.frame_type),
    @"showFrame" : @(header.show_frame != 0),
    @"showExistingFrame" : @(header.show_existing_frame != 0),
    @"width" : @(header.width),
    @"height" : @(header.height),
    @"renderWidth" : @(header.render_width),
    @"renderHeight" : @(header.render_height),
    @"baseQIdx" : @(header.base_q_idx),
    @"loopFilterLevel" : @(header.loop_filter_level),
    @"tileColsLog2" : @(header.tile_cols_log2),
    @"tileRowsLog2" : @(header.tile_rows_log2),
    @"headerSizeInBytes" : @(header.header_size_in_bytes),
  });
}

RCT_EXPORT_METHOD(parseAv1SequenceHeader
                  : (NSString *)obuBase64 resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  NSData *obu = decodeBase64(obuBase64, reject);
  if (obu == nil) {
    return;
  }
  codec_av1_sequence_header_t header;
  codec_error_t rc = codec_av1_parse_sequence_header(
      (const uint8_t *)obu.bytes, obu.length, &header);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  resolve(@{
    @"seqProfile" : @(header.seq_profile),
    @"seqLevelIdx0" : @(header.seq_level_idx_0),
    @"seqTier0" : @(header.seq_tier_0),
    @"maxFrameWidthMinus1" : @(header.max_frame_width_minus_1),
    @"maxFrameHeightMinus1" : @(header.max_frame_height_minus_1),
    @"frameWidthBitsMinus1" : @(header.frame_width_bits_minus_1),
    @"frameHeightBitsMinus1" : @(header.frame_height_bits_minus_1),
    @"reducedStillPictureHeader" :
        @(header.reduced_still_picture_header != 0),
  });
}

RCT_EXPORT_METHOD(encoderNewFlac
                  : (double)sampleRate channels
                  : (double)channels resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  codec_encoder_t *encoder = nullptr;
  codec_error_t rc = codec_encoder_new_flac((uint32_t)sampleRate,
                                            (uint16_t)channels, &encoder);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  @synchronized(encoders) {
    int32_t handle = nextEncoderHandle;
    nextEncoderHandle += 1;
    encoders[@(handle)] = [NSValue valueWithPointer:encoder];
    resolve(@(handle));
  }
}

RCT_EXPORT_METHOD(encoderStreaminfo
                  : (double)handle resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  codec_encoder_t *encoder = nullptr;
  @synchronized(encoders) {
    NSValue *stored = encoders[@((int32_t)handle)];
    if (stored != nil) {
      encoder = (codec_encoder_t *)stored.pointerValue;
    }
  }
  if (encoder == nullptr) {
    reject(@"7", @"unknown encoder handle", nil);
    return;
  }
  const codec_packet_t *packet = nullptr;
  codec_error_t rc = codec_encoder_flac_streaminfo(encoder, &packet);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  resolve(encodeBase64(packet->data, packet->len));
}

RCT_EXPORT_METHOD(encoderEncode
                  : (double)handle samples
                  : (NSArray<NSNumber *> *)samples frames
                  : (double)frames resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  codec_encoder_t *encoder = nullptr;
  @synchronized(encoders) {
    NSValue *stored = encoders[@((int32_t)handle)];
    if (stored != nil) {
      encoder = (codec_encoder_t *)stored.pointerValue;
    }
  }
  if (encoder == nullptr) {
    reject(@"7", @"unknown encoder handle", nil);
    return;
  }
  float *pcm = (float *)malloc(samples.count * sizeof(float));
  if (pcm == nullptr) {
    reject(@"6", @"allocation failed", nil);
    return;
  }
  for (NSUInteger i = 0; i < samples.count; i++) {
    pcm[i] = [samples[i] floatValue];
  }
  const codec_packet_t *packet = nullptr;
  codec_error_t rc =
      codec_encoder_encode(encoder, pcm, (uint64_t)frames, &packet);
  free(pcm);
  if (rc != CODEC_OK) {
    rejectCodec(reject, rc);
    return;
  }
  resolve(encodeBase64(packet->data, packet->len));
}

RCT_EXPORT_METHOD(encoderFree
                  : (double)handle resolver
                  : (RCTPromiseResolveBlock)resolve rejecter
                  : (RCTPromiseRejectBlock)reject) {
  codec_encoder_t *encoder = nullptr;
  @synchronized(encoders) {
    NSValue *stored = encoders[@((int32_t)handle)];
    if (stored != nil) {
      encoder = (codec_encoder_t *)stored.pointerValue;
      [encoders removeObjectForKey:@((int32_t)handle)];
    }
  }
  if (encoder == nullptr) {
    reject(@"7", @"unknown encoder handle", nil);
    return;
  }
  codec_encoder_free(encoder);
  resolve(nil);
}

- (void)invalidate {
  @synchronized(encoders) {
    for (NSValue *stored in encoders.allValues) {
      codec_encoder_free((codec_encoder_t *)stored.pointerValue);
    }
    [encoders removeAllObjects];
  }
  [super invalidate];
}

@end
