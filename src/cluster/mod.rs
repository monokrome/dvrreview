use crate::db::models::File;
use crate::scanner::fingerprint::Fingerprinter;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SimilarityMatch {
    pub file_a: Uuid,
    pub file_b: Uuid,
    pub score: f32,
}

pub struct ClusterBuilder {
    threshold: f32,
}

impl Default for ClusterBuilder {
    fn default() -> Self {
        Self::new(0.85)
    }
}

impl ClusterBuilder {
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }

    pub fn find_similar_pairs(
        &self,
        files: &[File],
        fingerprints: &HashMap<Uuid, Vec<Vec<u8>>>,
    ) -> Vec<SimilarityMatch> {
        let mut matches = Vec::new();

        for (i, file_a) in files.iter().enumerate() {
            for file_b in files.iter().skip(i + 1) {
                if let (Some(fps_a), Some(fps_b)) =
                    (fingerprints.get(&file_a.id), fingerprints.get(&file_b.id))
                {
                    let score = self.compare_fingerprint_sets(fps_a, fps_b);
                    if score >= self.threshold {
                        matches.push(SimilarityMatch {
                            file_a: file_a.id,
                            file_b: file_b.id,
                            score,
                        });
                    }
                }
            }
        }

        matches
    }

    fn compare_fingerprint_sets(&self, a: &[Vec<u8>], b: &[Vec<u8>]) -> f32 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }

        // Compare corresponding timestamps and average the similarity
        let mut total_similarity = 0.0;
        let mut count = 0;

        for (hash_a, hash_b) in a.iter().zip(b.iter()) {
            total_similarity += Fingerprinter::similarity(hash_a, hash_b);
            count += 1;
        }

        if count == 0 {
            0.0
        } else {
            total_similarity / count as f32
        }
    }

    pub fn build_clusters(&self, matches: &[SimilarityMatch]) -> Vec<Vec<Uuid>> {
        // Union-find to group connected files
        let mut parent: HashMap<Uuid, Uuid> = HashMap::new();

        fn find(parent: &mut HashMap<Uuid, Uuid>, x: Uuid) -> Uuid {
            if !parent.contains_key(&x) {
                parent.insert(x, x);
                return x;
            }
            let p = parent[&x];
            if p != x {
                let root = find(parent, p);
                parent.insert(x, root);
                return root;
            }
            x
        }

        fn union(parent: &mut HashMap<Uuid, Uuid>, a: Uuid, b: Uuid) {
            let root_a = find(parent, a);
            let root_b = find(parent, b);
            if root_a != root_b {
                parent.insert(root_a, root_b);
            }
        }

        for m in matches {
            union(&mut parent, m.file_a, m.file_b);
        }

        // Group by root
        let mut groups: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        let ids: Vec<Uuid> = parent.keys().copied().collect();
        for id in ids {
            let root = find(&mut parent, id);
            groups.entry(root).or_default().push(id);
        }

        groups
            .into_values()
            .filter(|g| g.len() > 1)
            .collect()
    }
}

pub fn rank_by_quality(files: &[File]) -> Vec<&File> {
    let mut ranked: Vec<_> = files.iter().collect();
    ranked.sort_by(|a, b| {
        // Higher resolution is better
        let res_a = a.width.unwrap_or(0) * a.height.unwrap_or(0);
        let res_b = b.width.unwrap_or(0) * b.height.unwrap_or(0);
        let res_cmp = res_b.cmp(&res_a);
        if res_cmp != std::cmp::Ordering::Equal {
            return res_cmp;
        }

        // Higher bitrate is better
        let br_cmp = b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0));
        if br_cmp != std::cmp::Ordering::Equal {
            return br_cmp;
        }

        // Larger file size as tiebreaker
        b.size_bytes.cmp(&a.size_bytes)
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_clusters() {
        let builder = ClusterBuilder::new(0.85);
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let id4 = Uuid::new_v4();

        let matches = vec![
            SimilarityMatch {
                file_a: id1,
                file_b: id2,
                score: 0.9,
            },
            SimilarityMatch {
                file_a: id2,
                file_b: id3,
                score: 0.88,
            },
            // id4 not connected
        ];

        let clusters = builder.build_clusters(&matches);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].len(), 3);
        assert!(!clusters.iter().any(|c| c.contains(&id4)));
    }
}
