def edit(p, pairs):
    s=open(p,encoding='utf-8').read()
    for pair in pairs:
        old,new=pair[0],pair[1]
        assert old in s, (p, old[:80])
        s=s.replace(old,new,1)
    open(p,'w',encoding='utf-8').write(s)

edit('apps/desktop/src/strings.rs',[
("    pub share_quality: &'static str,\n","    pub share_audio: &'static str,\n    pub share_audio_hint: &'static str,\n    pub share_quality: &'static str,\n"),
('    share_quality: "Qualidade",\n','    share_audio: "Enviar o som do computador",\n    share_audio_hint: "Tudo que toca no computador, menos o próprio Mirage, vai junto com a sua voz. Com o microfone silenciado no servidor, o som não chega.",\n    share_quality: "Qualidade",\n'),
])
edit('apps/desktop/src/protocol.rs',[
("""        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<&'static str>,
    },""","""        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<&'static str>,
        /// Send the computer's sound in our voice.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        audio: bool,
    },"""),
("                window: None,\n                quality: None\n            })","                window: None,\n                quality: None,\n                audio: false\n            })"),
("                quality: Some(\"high\")\n            })\n            .unwrap(),\n            serde_json::json!({\"op\": \"share_start\", \"monitor\": null, \"window\": 65678, \"quality\": \"high\"})",
 "                quality: Some(\"high\"),\n                audio: true\n            })\n            .unwrap(),\n            serde_json::json!({\"op\": \"share_start\", \"monitor\": null, \"window\": 65678, \"quality\": \"high\", \"audio\": true})"),
])
edit('apps/desktop/src/app.rs',[
("    share_quality: usize,\n","    share_quality: usize,\n    /// Send the computer's sound with the next share (not saved).\n    share_audio: bool,\n"),
("            share_quality: share::DEFAULT_QUALITY,\n","            share_quality: share::DEFAULT_QUALITY,\n            share_audio: false,\n"),
])
edit('apps/desktop/src/app/share.rs',[
("""                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(egui::RichText::new(t.share_pick)""","""                    ui.add_space(4.0);
                    ui.checkbox(&mut self.share_audio, t.share_audio)
                        .on_hover_text(t.share_audio_hint);
                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(egui::RichText::new(t.share_pick)"""),
("                        quality: QUALITIES.get(self.share_quality).copied(),\n","                        quality: QUALITIES.get(self.share_quality).copied(),\n                        audio: self.share_audio,\n"),
])
