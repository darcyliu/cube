//! Reserved dynamic MMIO aperture, disjoint from fixed platform devices.
//! Keep disk zero at its firmware-compatible address; move only extra disks.
pub fn dynamic_devices(disks: usize, shares: usize) -> Result<Vec<(u64, u32)>, String> {
    let n = disks
        .saturating_sub(1)
        .checked_add(shares)
        .ok_or("too many devices")?;
    if n > 64 {
        return Err("at most 64 additional disks and shared folders are supported".into());
    }
    Ok((0..n)
        .map(|i| (0x0a02_0000 + i as u64 * 0x200, 48 + i as u32))
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamic_ranges_avoid_graphics_and_bound_interrupts() {
        let slots = dynamic_devices(4, 3).unwrap();
        assert_eq!(slots.len(), 6);
        for (i, &(base, irq)) in slots.iter().enumerate() {
            assert!(base >= 0x0a02_0000 && irq > 41);
            for &(other, other_irq) in &slots[..i] {
                assert!(base >= other + 0x200);
                assert_ne!(irq, other_irq);
            }
        }
        assert!(dynamic_devices(65, 1).is_err());
    }
}
