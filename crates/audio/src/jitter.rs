//! Адаптивный джиттер-буфер для UDP медиа-потока.
//!
//! - сортирует пакеты по seq, выбрасывает опоздавшие и дубли
//! - адаптивная глубина 2..8 фреймов (40..160 мс), старт — после накопления target фреймов
//! - при одиночной дыре отдаёт Fec(следующий пакет) — Opus восстановит потерянный фрейм
//! - при полном опустошении отдаёт Plc — декодер сгенерирует комфортный шум

use std::collections::BTreeMap;

const MAX_BUFFERED: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Pop {
    /// Обычный пакет к декодированию
    Packet(Vec<u8>),
    /// Пакет seq+1: декодировать с fec=true для восстановления потерянного фрейма
    Fec(Vec<u8>),
    /// Пакета нет: декодировать с fec=false и None (PLC)
    Plc,
}

pub struct JitterBuffer {
    frames: BTreeMap<u32, Vec<u8>>,
    next_seq: Option<u32>,
    target: usize,
    min_target: usize,
    max_target: usize,
    underruns: u32,
    good_pops: u32,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new(3)
    }
}

impl JitterBuffer {
    pub fn new(target: usize) -> Self {
        Self {
            frames: BTreeMap::new(),
            next_seq: None,
            target,
            min_target: 2,
            max_target: 8,
            underruns: 0,
            good_pops: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn target(&self) -> usize {
        self.target
    }

    pub fn push(&mut self, seq: u32, payload: Vec<u8>) {
        // опоздавший пакет — выбрасываем
        if let Some(ns) = self.next_seq {
            if seq_lt(seq, ns) {
                return;
            }
        }
        // дубликаты игнорируем — выигрывает пакет, пришедший первым
        self.frames.entry(seq).or_insert(payload);
        // защита от разрастания: выбрасываем самые старые
        while self.frames.len() > MAX_BUFFERED {
            if let Some(&first) = self.frames.keys().next() {
                self.frames.remove(&first);
            }
        }
    }

    pub fn pop(&mut self) -> Pop {
        // старт воспроизведения — после накопления target фреймов
        if self.next_seq.is_none() {
            if self.frames.len() < self.target {
                return Pop::Plc;
            }
            self.next_seq = self.frames.keys().next().copied();
        }
        let ns = self.next_seq.unwrap();

        if let Some(data) = self.frames.remove(&ns) {
            self.next_seq = Some(ns.wrapping_add(1));
            self.on_good();
            return Pop::Packet(data);
        }

        // одиночная дыра: восстановим через FEC следующего пакета
        let nxt = ns.wrapping_add(1);
        if let Some(next_data) = self.frames.get(&nxt) {
            let fec_src = next_data.clone();
            self.next_seq = Some(nxt);
            self.on_good();
            return Pop::Fec(fec_src);
        }

        if self.frames.is_empty() {
            self.on_underrun();
            return Pop::Plc;
        }

        // несколько потерь подряд — перескакиваем на ближайший имеющийся
        let first = *self.frames.keys().next().unwrap();
        self.next_seq = Some(first.wrapping_add(1));
        self.on_underrun();
        Pop::Packet(self.frames.remove(&first).unwrap())
    }

    fn on_underrun(&mut self) {
        self.underruns += 1;
        self.good_pops = 0;
        // частые underrun — увеличиваем глубину буфера
        if self.underruns >= 5 && self.target < self.max_target {
            self.target += 1;
            self.underruns = 0;
        }
    }

    fn on_good(&mut self) {
        self.underruns = 0;
        self.good_pops += 1;
        // долго без потерь — уменьшаем задержку
        if self.good_pops >= 500 && self.target > self.min_target {
            self.target -= 1;
            self.good_pops = 0;
        }
    }
}

fn seq_lt(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) > u32::MAX / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(n: u32) -> Vec<u8> {
        vec![(n % 256) as u8]
    }

    #[test]
    fn test_starts_after_target_depth() {
        let mut jb = JitterBuffer::new(3);
        jb.push(1, pkt(1));
        jb.push(2, pkt(2));
        assert_eq!(jb.pop(), Pop::Plc); // ещё не накопили
        jb.push(3, pkt(3));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(3)));
    }

    #[test]
    fn test_reorders_out_of_order() {
        let mut jb = JitterBuffer::new(2);
        jb.push(2, pkt(2));
        jb.push(1, pkt(1));
        jb.push(3, pkt(3));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(3)));
    }

    #[test]
    fn test_drops_late_packets() {
        let mut jb = JitterBuffer::new(2);
        jb.push(10, pkt(10));
        jb.push(11, pkt(11));
        assert_eq!(jb.pop(), Pop::Packet(pkt(10)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(11)));
        jb.push(5, pkt(5)); // опоздал — должен быть выброшен
        assert_eq!(jb.pop(), Pop::Plc);
    }

    #[test]
    fn test_single_loss_uses_fec() {
        let mut jb = JitterBuffer::new(2);
        jb.push(1, pkt(1));
        jb.push(2, pkt(2));
        jb.push(4, pkt(4)); // пакет 3 потерян
        jb.push(5, pkt(5));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.pop(), Pop::Fec(pkt(4))); // восстановление 3-го из 4-го
        assert_eq!(jb.pop(), Pop::Packet(pkt(4)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(5)));
    }

    #[test]
    fn test_burst_loss_skips_forward() {
        let mut jb = JitterBuffer::new(2);
        jb.push(1, pkt(1));
        jb.push(2, pkt(2));
        jb.push(10, pkt(10)); // 3..9 потеряны пачкой
        assert_eq!(jb.pop(), Pop::Packet(pkt(1)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(2)));
        assert_eq!(jb.pop(), Pop::Packet(pkt(10)));
    }

    #[test]
    fn test_underrun_gives_plc() {
        let mut jb = JitterBuffer::new(2);
        jb.push(1, pkt(1));
        jb.push(2, pkt(2));
        let _ = jb.pop();
        let _ = jb.pop();
        assert_eq!(jb.pop(), Pop::Plc);
        assert_eq!(jb.pop(), Pop::Plc);
    }

    #[test]
    fn test_target_grows_on_underruns() {
        let mut jb = JitterBuffer::new(2);
        assert_eq!(jb.target(), 2);
        // запускаем буфер и опустошаем
        jb.push(1, pkt(1));
        jb.push(2, pkt(2));
        let _ = jb.pop();
        let _ = jb.pop();
        // 5 подряд underrun после старта → глубина растёт
        for _ in 0..5 {
            let _ = jb.pop();
        }
        assert!(jb.target() > 2);
    }

    #[test]
    fn test_duplicate_ignored() {
        let mut jb = JitterBuffer::new(2);
        jb.push(1, pkt(1));
        jb.push(1, pkt(99));
        jb.push(2, pkt(2));
        assert_eq!(jb.pop(), Pop::Packet(pkt(1))); // первая копия выиграла
    }
}
