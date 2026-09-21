from .ring import LeaseStillBorrowed, PublishResult, RingError, RingFull, RingTimeout, SharedTensorRing, TensorConsumer, TensorLease

from .protocol import read_u32 as _read_u32  # noqa: F401

#: Maximum number of simultaneous consumers a single ring supports.
#: Fixed by the protocol header at ring creation; attaching a consumer
#: beyond this raises RingError("maximum consumer count reached").
MAX_CONSUMERS = 64

__version__ = "0.1.0a0"
__all__ = [
    "SharedTensorRing", "TensorConsumer", "TensorLease", "PublishResult",
    "RingError", "LeaseStillBorrowed", "MAX_CONSUMERS", "RingFull", "RingTimeout",
]
