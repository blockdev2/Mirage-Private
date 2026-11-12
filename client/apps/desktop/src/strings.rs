//! User-facing text for the desktop client.
//!
//! Every label the user can read lives here, so a second language is a second
//! table rather than a search through the views. Only Portuguese exists today.
//! Code comments stay in English.

/// Static labels. Dynamic phrases are methods so each language controls word order.
pub struct Strings {
    // Navigation and sidebar.
    pub nav_home: &'static str,
    pub nav_history: &'static str,
    pub nav_voice: &'static str,
    pub channels_heading: &'static str,
    pub no_channels_hint: &'static str,
    pub favorites_heading: &'static str,
    pub favorites_selected: &'static str,
    pub add_server: &'static str,
    pub no_server: &'static str,
    pub private_server: &'static str,
    pub offline: &'static str,
    pub you: &'static str,
    pub available: &'static str,
    pub disconnected: &'static str,
    pub local_profile: &'static str,
    pub no_profile: &'static str,

    // Home screen.
    pub home_title: &'static str,
    pub home_subtitle: &'static str,
    pub search_placeholder: &'static str,
    pub last_connection: &'static str,
    pub connect_again: &'static str,
    pub favorited: &'static str,
    pub recents: &'static str,
    pub view_history: &'static str,
    pub history_title: &'static str,
    pub history_subtitle: &'static str,
    pub clear_history: &'static str,
    pub no_history_title: &'static str,
    pub no_history_body: &'static str,
    pub no_results: &'static str,
    pub no_favorites: &'static str,
    pub last_access_prefix: &'static str,
    pub time_just_now: &'static str,
    pub time_minutes: &'static str,
    pub time_hours: &'static str,
    pub time_yesterday: &'static str,
    pub time_days: &'static str,

    // Quick connect.
    pub quick_connect: &'static str,
    pub quick_connect_intro: &'static str,
    pub use_saved_profile: &'static str,
    pub server_address: &'static str,
    pub address_hint: &'static str,
    pub address_empty: &'static str,
    pub address_spaces: &'static str,
    pub address_port: &'static str,
    pub join_as: &'static str,
    pub profile_saved_note: &'static str,
    pub use_other_nickname: &'static str,
    pub other_nickname_label: &'static str,
    pub server_password_toggle: &'static str,
    pub server_password_hint: &'static str,
    pub save_to_favorites: &'static str,
    pub connect: &'static str,
    pub connecting: &'static str,
    pub busy_in_call: &'static str,
    pub need_profile: &'static str,
    pub error_banned: &'static str,
    pub error_password: &'static str,
    pub bridge_offline: &'static str,
    pub bridge_offline_short: &'static str,
    pub ready_to_connect: &'static str,

    // Local profiles.
    pub profile_panel_title: &'static str,
    pub profile_panel_body: &'static str,
    pub default_profile: &'static str,
    pub edit_profile: &'static str,
    pub new_profile: &'static str,
    pub profile_first_title: &'static str,
    pub profile_first_body: &'static str,
    pub nickname_label: &'static str,
    pub nickname_required: &'static str,
    pub make_default: &'static str,
    pub delete_profile: &'static str,
    pub choose_photo: &'static str,
    pub change_photo: &'static str,
    pub remove_photo: &'static str,
    pub photo_hint: &'static str,
    pub photo_picker_title: &'static str,
    pub photo_filter: &'static str,
    pub photo_too_large: &'static str,
    pub photo_unreadable: &'static str,
    pub photo_save_failed: &'static str,
    pub delete_profile_body: &'static str,
    pub delete: &'static str,
    pub clear_history_body: &'static str,
    pub clear_confirm: &'static str,
    pub create_profile: &'static str,
    pub results: &'static str,
    pub save_failed: &'static str,

    // Menus and dialogs.
    pub menu_connect: &'static str,
    pub menu_edit: &'static str,
    pub menu_add_favorite: &'static str,
    pub menu_remove_favorite: &'static str,
    pub menu_remove_history: &'static str,
    pub favorite_add_title: &'static str,
    pub favorite_edit_title: &'static str,
    pub favorite_name: &'static str,
    pub favorite_name_hint: &'static str,
    pub save: &'static str,
    pub cancel: &'static str,

    // Voice session.
    pub no_channel: &'static str,
    pub channel_kind: &'static str,
    pub participant_one: &'static str,
    pub participant_many: &'static str,
    pub view_list: &'static str,
    pub view_grid: &'static str,
    pub invite: &'static str,
    pub invite_disabled: &'static str,
    pub default_topic: &'static str,
    pub chat_heading: &'static str,
    pub chat_send: &'static str,
    pub chat_limit: &'static str,
    pub chat_uncertain: &'static str,
    pub chat_empty_title: &'static str,
    pub chat_empty_body: &'static str,
    pub chat_unavailable_title: &'static str,
    pub chat_unavailable_body: &'static str,
    pub chat_placeholder_prefix: &'static str,
    pub chat_disabled: &'static str,
    pub voice_connected: &'static str,
    pub push_to_talk: &'static str,
    pub mic_mute: &'static str,
    pub mic_unmute: &'static str,
    pub output_mute: &'static str,
    pub output_unmute: &'static str,
    pub share_screen: &'static str,
    pub share_disabled: &'static str,
    pub more_options: &'static str,
    pub leave_call: &'static str,
    pub logo_fallback: &'static str,
    pub muted_state: &'static str,
    pub speaking: &'static str,
    pub listening: &'static str,

    // Settings window and diagnostics.
    pub settings_title: &'static str,
    pub audio_section: &'static str,
    pub enable_audio: &'static str,
    pub transmit_mode: &'static str,
    pub mode_voice: &'static str,
    pub mode_ptt: &'static str,
    pub mode_continuous: &'static str,
    pub vad_threshold: &'static str,
    pub vad_hint: &'static str,
    pub ptt_hint: &'static str,
    pub noise_title: &'static str,
    pub user_volume: &'static str,
    pub user_mute_local: &'static str,
    pub user_unmute_local: &'static str,
    pub user_reset_volume: &'static str,
    pub user_connection_info: &'static str,
    pub info_loss_in: &'static str,
    pub info_loss_out: &'static str,
    pub info_ping: &'static str,
    pub info_idle: &'static str,
    pub info_connected: &'static str,
    pub info_bytes_in: &'static str,
    pub info_bytes_out: &'static str,
    pub info_address: &'static str,
    pub info_hidden: &'static str,
    pub info_waiting: &'static str,
    pub info_copy: &'static str,
    pub noise_off: &'static str,
    pub noise_standard: &'static str,
    pub noise_strong: &'static str,
    pub noise_hint_off: &'static str,
    pub noise_hint_standard: &'static str,
    pub noise_hint_strong: &'static str,
    pub input: &'static str,
    pub output: &'static str,
    pub system_default: &'static str,
    pub ptt_key: &'static str,
    pub apply_audio: &'static str,
    pub refresh_devices: &'static str,
    pub profile_section: &'static str,
    pub save_nickname: &'static str,
    pub show_diagnostics: &'static str,
    pub diagnostics: &'static str,
    pub clear: &'static str,
    pub bridge_version_prefix: &'static str,
}

impl Strings {
    /// "1 participante" / "2 participantes".
    pub fn participants_count(&self, count: usize) -> String {
        let noun = if count == 1 {
            self.participant_one
        } else {
            self.participant_many
        };
        format!("{count} {noun}")
    }

    /// "Server / Voice channels" breadcrumb.
    pub fn breadcrumb(&self, server: &str) -> String {
        format!("{server} / {}", self.nav_voice)
    }

    /// "Voice channel • topic" line under the channel title.
    pub fn channel_subtitle(&self, topic: &str) -> String {
        format!("{} • {topic}", self.channel_kind)
    }

    /// Placeholder of the chat composer, naming the channel.
    pub fn chat_placeholder_in(&self, channel: &str) -> String {
        format!("{} {channel}", self.chat_placeholder_prefix)
    }

    /// Caption under the address field when a favorite is selected.
    pub fn selected_favorite(&self, name: &str) -> String {
        format!("{}: {name}", self.favorites_selected)
    }

    /// "Último acesso: há 5 min", from how long ago the server was used.
    pub fn last_access(&self, seconds_ago: u64) -> String {
        let when = if seconds_ago < 60 {
            self.time_just_now.to_string()
        } else if seconds_ago < 3_600 {
            self.time_minutes
                .replace("{n}", &(seconds_ago / 60).to_string())
        } else if seconds_ago < 86_400 {
            self.time_hours
                .replace("{n}", &(seconds_ago / 3_600).to_string())
        } else if seconds_ago < 2 * 86_400 {
            self.time_yesterday.to_string()
        } else {
            self.time_days
                .replace("{n}", &(seconds_ago / 86_400).to_string())
        };
        format!("{}: {when}", self.last_access_prefix)
    }

    /// Friendly text for the connection errors we recognise; anything else
    /// is shown as the server reported it.
    pub fn connect_error(&self, raw: &str) -> String {
        if raw.contains("ConnectFailedBanned") {
            self.error_banned.to_string()
        } else if raw.to_lowercase().contains("password") {
            self.error_password.to_string()
        } else {
            raw.to_string()
        }
    }

    pub fn bridge_version(&self, version: &str) -> String {
        format!("{} {version}", self.bridge_version_prefix)
    }
}

/// Portuguese (Brazil), the only language shipped for now.
pub const PT: Strings = Strings {
    nav_home: "Início",
    nav_history: "Histórico",
    nav_voice: "Canais de voz",
    channels_heading: "Canais de voz",
    no_channels_hint: "Conecte-se a um servidor para ver os canais.",
    favorites_heading: "Favoritos",
    favorites_selected: "Favorito selecionado",
    add_server: "Adicionar servidor",
    no_server: "Nenhum servidor",
    private_server: "Servidor privado",
    offline: "Offline",
    you: "Você",
    available: "Disponível",
    disconnected: "Desconectado",
    local_profile: "Perfil local",
    no_profile: "Sem perfil",

    home_title: "Seus servidores",
    home_subtitle: "Entre na conversa.",
    search_placeholder: "Buscar servidor",
    last_connection: "Última conexão",
    connect_again: "Conectar novamente",
    favorited: "Favoritado",
    recents: "Recentes",
    view_history: "Ver histórico",
    history_title: "Histórico",
    history_subtitle: "Servidores em que você entrou neste computador.",
    clear_history: "Limpar histórico",
    no_history_title: "Nenhuma conexão ainda",
    no_history_body: "Use a conexão rápida para entrar no primeiro servidor. Ele aparecerá aqui.",
    no_results: "Nada encontrado.",
    no_favorites: "Nenhum favorito ainda.",
    last_access_prefix: "Último acesso",
    time_just_now: "agora há pouco",
    time_minutes: "há {n} min",
    time_hours: "há {n} h",
    time_yesterday: "ontem",
    time_days: "há {n} dias",

    quick_connect: "Conexão rápida",
    quick_connect_intro: "Entre com o endereço de um servidor.",
    use_saved_profile: "Usar o perfil salvo",
    server_address: "Endereço do servidor",
    address_hint: "servidor.exemplo.com",
    address_empty: "Informe o endereço do servidor.",
    address_spaces: "O endereço não pode ter espaços.",
    address_port: "A porta deve ser um número entre 1 e 65535.",
    join_as: "Entrar como",
    profile_saved_note: "Perfil salvo neste computador.",
    use_other_nickname: "Usar outro apelido",
    other_nickname_label: "Apelido só para esta conexão",
    server_password_toggle: "Senha do servidor",
    server_password_hint: "Só se o servidor exigir. Ela não é salva.",
    save_to_favorites: "Salvar nos favoritos",
    connect: "Conectar",
    connecting: "Conectando…",
    busy_in_call: "Você já está em uma chamada. Saia dela para conectar a outro servidor.",
    need_profile: "Crie um perfil para poder conectar.",
    error_banned: "O servidor recusou a conexão: este endereço está bloqueado no momento, \
                   normalmente por tentativas de conexão em sequência. Aguarde alguns \
                   minutos antes de tentar de novo.",
    error_password: "O servidor pediu uma senha que não foi aceita. Confira a senha e tente \
                     de novo.",
    bridge_offline: "O serviço local do Mirage não está em execução (127.0.0.1:8787). Inicie o \
                     ts6-bridge e tente de novo.",
    bridge_offline_short: "Serviço local offline",
    ready_to_connect: "Pronto para conectar",

    profile_panel_title: "Seu perfil neste PC",
    profile_panel_body: "Seu apelido fica salvo neste computador.",
    default_profile: "Perfil padrão",
    edit_profile: "Editar perfil",
    new_profile: "Novo perfil",
    profile_first_title: "Crie seu perfil local",
    profile_first_body: "Seu apelido fica salvo neste computador. Não é preciso criar conta.",
    nickname_label: "Apelido",
    nickname_required: "Informe um apelido.",
    make_default: "Definir como perfil padrão",
    delete_profile: "Excluir perfil",
    choose_photo: "Escolher foto",
    change_photo: "Trocar foto",
    remove_photo: "Remover foto",
    photo_hint: "PNG, JPG, GIF, BMP, WEBP, TIFF ou ICO. Fica salva só neste computador.",
    photo_picker_title: "Escolher foto de perfil",
    photo_filter: "Imagens",
    photo_too_large: "A imagem passa de 25 MB. Escolha um arquivo menor.",
    photo_unreadable: "Não foi possível abrir esta imagem. Escolha outro arquivo.",
    photo_save_failed: "Não foi possível salvar a foto neste computador.",
    delete_profile_body: "O perfil será removido deste computador. Favoritos e histórico \
                          continuam salvos.",
    delete: "Excluir",
    clear_history_body: "O histórico será apagado deste computador. Seus favoritos não são \
                         afetados.",
    clear_confirm: "Limpar",
    create_profile: "Criar perfil",
    results: "Resultados",
    save_failed: "Não foi possível salvar os dados neste computador.",

    menu_connect: "Conectar",
    menu_edit: "Editar",
    menu_add_favorite: "Adicionar aos favoritos",
    menu_remove_favorite: "Remover dos favoritos",
    menu_remove_history: "Remover do histórico",
    favorite_add_title: "Adicionar servidor",
    favorite_edit_title: "Editar favorito",
    favorite_name: "Nome",
    favorite_name_hint: "Nome do servidor",
    save: "Salvar",
    cancel: "Cancelar",

    no_channel: "Sem canal",
    channel_kind: "Canal de voz",
    participant_one: "participante",
    participant_many: "participantes",
    view_list: "Lista",
    view_grid: "Grade",
    invite: "Convidar",
    invite_disabled: "Convites ainda não estão disponíveis.",
    default_topic: "Um espaço para conversar e construir.",
    chat_heading: "Conversa da sala",
    chat_send: "Enviar",
    chat_limit: "Limite: 1024 bytes UTF-8.",
    chat_uncertain:
        "Envio sem confirma\u{e7}\u{e3}o. Confira a conversa antes de tentar novamente.",
    chat_empty_title: "Ainda sem mensagens",
    chat_empty_body: "Comece a conversa com quem está na sala.",
    chat_unavailable_title: "Chat indisponível",
    chat_unavailable_body: "A conversa por texto não está disponível nesta conexão.",
    chat_placeholder_prefix: "Conversar em",
    chat_disabled: "A conversa por texto não está disponível nesta conexão.",
    voice_connected: "Voz conectada",
    push_to_talk: "Push-to-talk",
    mic_mute: "Silenciar microfone",
    mic_unmute: "Ativar microfone",
    output_mute: "Silenciar áudio",
    output_unmute: "Ativar áudio",
    share_screen: "Compartilhar tela",
    share_disabled: "Compartilhamento de tela ainda não está implementado.",
    more_options: "Mais opções",
    leave_call: "Sair da chamada",
    logo_fallback: "Mirage",
    muted_state: "Mudo",
    speaking: "Falando",
    listening: "Ouvindo",

    settings_title: "Ajustes",
    audio_section: "Áudio",
    enable_audio: "Ativar áudio",
    transmit_mode: "Modo de transmissão",
    mode_voice: "Ativação por voz",
    mode_ptt: "Push-to-talk",
    mode_continuous: "Contínuo",
    vad_threshold: "Sensibilidade do microfone",
    vad_hint: "Fale normalmente: a barra deve passar da marca só quando você fala.",
    noise_title: "Redução de ruído",
    user_volume: "Volume do usuário",
    user_mute_local: "Silenciar para mim",
    user_unmute_local: "Voltar a ouvir",
    user_reset_volume: "Restaurar 100%",
    user_connection_info: "Informações de conexão",
    info_loss_in: "Perda de pacote (recebimento)",
    info_loss_out: "Perda de pacote (saída)",
    info_ping: "Ping",
    info_idle: "Tempo ocioso",
    info_connected: "Conectado há",
    info_bytes_in: "Bytes recebidos",
    info_bytes_out: "Bytes enviados",
    info_address: "Endereço IP",
    info_hidden: "Oculto pelo servidor",
    info_waiting: "Aguardando o servidor…",
    info_copy: "Copiar endereço",
    noise_off: "Desligada",
    noise_standard: "Padrão",
    noise_strong: "Forte",
    noise_hint_off: "O som do microfone vai sem tratamento.",
    noise_hint_standard: "Remove ruído constante: ventilador, chiado, zumbido, trânsito.",
    noise_hint_strong: "Também corta o que não é voz: teclado, cliques do mouse e respiração.",
    ptt_hint: "Tecla segurada para falar, por exemplo F13, V ou Mouse4.",
    input: "Microfone",
    output: "Headset",
    system_default: "Padrão do sistema",
    ptt_key: "Tecla push-to-talk",
    apply_audio: "Aplicar áudio",
    refresh_devices: "Atualizar dispositivos",
    profile_section: "Perfil",
    save_nickname: "Salvar apelido",
    show_diagnostics: "Mostrar diagnóstico",
    diagnostics: "Diagnóstico",
    clear: "Limpar",
    bridge_version_prefix: "Bridge",
};

#[cfg(test)]
mod tests {
    use super::PT;

    #[test]
    fn last_access_uses_the_right_unit() {
        assert_eq!(PT.last_access(10), "Último acesso: agora há pouco");
        assert_eq!(PT.last_access(5 * 60), "Último acesso: há 5 min");
        assert_eq!(PT.last_access(3 * 3_600), "Último acesso: há 3 h");
        assert_eq!(PT.last_access(30 * 3_600), "Último acesso: ontem");
        assert_eq!(PT.last_access(4 * 86_400), "Último acesso: há 4 dias");
    }

    #[test]
    fn password_errors_are_translated() {
        assert_eq!(PT.connect_error("wrong Password"), PT.error_password);
        assert_eq!(PT.connect_error("boom"), "boom");
    }
}
