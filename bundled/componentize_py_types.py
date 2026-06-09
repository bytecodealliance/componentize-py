from typing import TypeVar, Generic, Union
from dataclasses import dataclass

S = TypeVar('S')
@dataclass
class Some(Generic[S]):
    """Represents the "present" (i.e. non-absent) case of an optional value.

    This is used to disambiguate values of a nested ComponentModel `option` type
    (e.g. `option<option<T>>` or similar).  Non-nested `option` values are
    represented using `typing.Optional`, i.e. nullable types.

    """
    value: S

T = TypeVar('T')
@dataclass
class Ok(Generic[T]):
    """Represents the success case of a Component Model `result` value."""
    value: T

E = TypeVar('E')
@dataclass(frozen=True)
class Err(Generic[E], Exception):
    """Represents the failure case of a Component Model `result` value."""
    value: E

Result = Union[Ok[T], Err[E]]
"""Represents a Component Model `result` value, i.e. a variant type representing
either success payload or failure payload.

"""
