// Based on https://github.com/rust-random/rand/blob/master/rand_pcg/src/pcg64.rs, which is
//
// Copyright 2018 Developers of the Rand project.
// Copyright 2017 Paul Dicker.
// Copyright 2014-2017 Melissa O'Neill and PCG Project contributors
//
// and licensed under the MIT license.

use core::fmt;
use rand_core::{impls, Error, RngCore};
#[cfg(feature = "serde1")]
use serde::{Deserialize, Serialize};

// This is the default multiplier used by PCG for 64-bit state.
const MULTIPLIER: u64 = 0x5851f42d4c957f2d;
const INCREMENT: u64 = 0xb47c73972972b7b7;
const INITIAL_OFFSET: u64 = 0x3d657cc62bc341e;

/// A PCG random number generator (XSH RR 64/32 (LCG) variant).
///
/// Permuted Congruential Generator with 64-bit state, internal Linear
/// Congruential Generator, and 32-bit output via "xorshift high (bits),
/// random rotation" output function.
///
/// This is a 64-bit LCG with explicitly chosen stream with the PCG-XSH-RR
/// output function. This combination is the standard `pcg32`.
///
/// Despite the name, this implementation uses 16 bytes (128 bit) space
/// comprising 64 bits of state and 64 bits stream selector. These are both set
/// by `SeedableRng`, using a 128-bit seed.
///
/// Note that two generators with different stream parameter may be closely
/// correlated.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde1", derive(Serialize, Deserialize))]
pub struct SggPcg {
    state: u64,
}

impl SggPcg {
    /// Multi-step advance functions (jump-ahead, jump-back)
    ///
    /// The method used here is based on Brown, "Random Number Generation
    /// with Arbitrary Stride,", Transactions of the American Nuclear
    /// Society (Nov. 1994).  The algorithm is very similar to fast
    /// exponentiation.
    ///
    /// Even though delta is an unsigned integer, we can pass a
    /// signed integer to go backwards, it just goes "the long way round".
    ///
    /// Using this function is equivalent to calling `next_32()` `delta`
    /// number of times.
    pub fn new(seed: u64) -> Self {
        SggPcg {
            state: seed.wrapping_mul(MULTIPLIER).wrapping_sub(INITIAL_OFFSET),
        }
    }

    #[inline]
    fn step(&mut self) {
        // prepare the LCG for the next round
        self.state = self.state.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
    }
}

// Custom Debug implementation that does not expose the internal state
impl fmt::Debug for SggPcg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SggPcg {{}}")
    }
}

impl RngCore for SggPcg {
    #[inline]
    fn next_u32(&mut self) -> u32 {
        let state = self.state;
        self.step();

        // Output function XSH RR: xorshift high (bits), followed by a random rotate
        // Constants are for 64-bit state, 32-bit output
        const ROTATE: u32 = 59; // 64 - 5
        const XSHIFT: u32 = 18; // (5 + 32) / 2
        const SPARE: u32 = 27; // 64 - 32 - 5

        let rot = (state >> ROTATE) as u32;
        let xsh = (((state >> XSHIFT) ^ state) >> SPARE) as u32;
        xsh.rotate_right(rot)
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_u32(self)
    }

    #[inline]
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        impls::fill_bytes_via_next(self, dest)
    }

    #[inline]
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}
