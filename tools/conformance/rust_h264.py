"""Fluster decoder plugin for rust_h264.

run.sh copies this file into Fluster's decoders directory and sets
RUST_H264_DECODER to the conformance_decode example binary.
"""

import os
from typing import Any, Dict, Optional

from fluster.codec import Codec, OutputFormat
from fluster.decoder import Decoder, register_decoder
from fluster.utils import file_checksum, run_command


@register_decoder
class RustH264Decoder(Decoder):
    """rust_h264 software decoder (OrderedDecoder)"""

    name = "rust_h264"
    description = "rust_h264 H.264 decoder"
    codec = Codec.H264
    binary = os.environ.get("RUST_H264_DECODER", "conformance_decode")

    def decode(
        self,
        input_filepath: str,
        output_filepath: str,
        output_format: OutputFormat,
        timeout: int,
        verbose: bool,
        keep_files: bool,
        optional_params: Optional[Dict[str, Any]] = None,
    ) -> str:
        run_command([self.binary, input_filepath, output_filepath], timeout=timeout, verbose=verbose)
        return file_checksum(output_filepath)
