// Local, process-bound text generation. No socket, subprocess or remote API.
// Built against the pinned llama.cpp revision by scripts/build-llm.sh.
#include "llama.h"

#include <algorithm>
#include <chrono>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>
#ifdef __OHOS__
#include <hilog/log.h>
#include <sys/auxv.h>
#endif

namespace {
std::mutex model_mutex;
llama_model *cached_model = nullptr;
std::string cached_path;
thread_local std::string last_error;
using Clock = std::chrono::steady_clock;

void progress(const char *stage, Clock::time_point started, int tokens = 0) {
#ifdef __OHOS__
    const auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started).count();
    OH_LOG_Print(LOG_APP, LOG_INFO, 0, "DiaryLlm", "stage=%{public}s ms=%{public}lld tokens=%{public}d",
        stage, static_cast<long long>(ms), tokens);
#else
    (void)stage; (void)started; (void)tokens;
#endif
}

int fail(const char *message) {
    last_error = message;
    return -1;
}

bool set_batch(llama_batch_ext *batch, const llama_token *tokens, int count, int pos) {
    llama_batch_ext_clear(batch);
    for (int i = 0; i < count; ++i) {
        const int index = llama_batch_ext_add_token(batch, 0, tokens[i]);
        const llama_pos token_pos = pos + i;
        if (index < 0 || !llama_batch_ext_set_pos(batch, index, &token_pos)) return false;
    }
    return llama_batch_ext_set_output_logits(batch, count - 1, true);
}

struct Context {
    llama_context *ctx;
    llama_batch_ext *batch;
    llama_sampler *sampler;
    ~Context() {
        if (batch) llama_batch_ext_free(batch);
        if (sampler) llama_sampler_free(sampler);
        if (ctx) llama_free(ctx);
    }
};

std::string content(const char *value) {
    std::string result(value);
    size_t at = 0;
    while ((at = result.find("<|", at)) != std::string::npos) {
        result.replace(at, 2, "< | ");
        at += 4;
    }
    return result;
}
} // namespace

extern "C" const char *diary_llm_last_error() { return last_error.c_str(); }
extern "C" unsigned long diary_llm_cpu_features() {
#if defined(__OHOS__) && defined(__aarch64__)
    const unsigned long features = getauxval(AT_HWCAP);
    OH_LOG_Print(LOG_APP, LOG_INFO, 0, "DiaryLlm", "cpu_hwcap=%{public}lu", features);
    return features;
#else
    return 0;
#endif
}

extern "C" int diary_llm_generate(const char *model_path, const char *system,
                                    const char *user, int max_tokens,
                                    char *output, size_t output_capacity) {
    if (!model_path || !system || !user || !output || output_capacity < 2 ||
        max_tokens < 1 || max_tokens > 4096) return fail("invalid generation input");
    output[0] = '\0';
    std::lock_guard<std::mutex> lock(model_mutex);
    const auto started = Clock::now();
    progress("start", started);
    static const bool initialized = [] { llama_backend_init(); return true; }();
    (void)initialized;
    if (!cached_model || cached_path != model_path) {
        if (cached_model) llama_model_free(cached_model);
        cached_model = nullptr;
        cached_path.clear();
        auto params = llama_model_default_params();
        params.n_gpu_layers = 0;
        cached_model = llama_model_load_from_file(model_path, params);
        if (!cached_model) return fail("model load failed");
        cached_path = model_path;
    }
    progress("model_ready", started);

    // Qwen3.5's official enable_thinking=false template ends the assistant
    // prefix with an empty thinking block. Keep reasoning out of saved diaries.
    // diary_core supplies instructions; output never becomes an app instruction.
    const std::string prompt = std::string("<|im_start|>system\n") + content(system) +
        "<|im_end|>\n<|im_start|>user\n" + content(user) +
        "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n";
    const llama_vocab *vocab = llama_model_get_vocab(cached_model);
    int n_prompt = -llama_tokenize(vocab, prompt.c_str(), prompt.size(), nullptr, 0, true, true);
    if (n_prompt <= 0 || n_prompt > 12288) return fail("prompt too long");
    progress("tokenized", started, n_prompt);
    std::vector<llama_token> tokens(static_cast<size_t>(n_prompt));
    if (llama_tokenize(vocab, prompt.c_str(), prompt.size(), tokens.data(), n_prompt, true, true) != n_prompt)
        return fail("tokenization failed");

    auto ctx_params = llama_context_default_params();
    ctx_params.n_ctx = std::max(4096, n_prompt + max_tokens);
    ctx_params.n_batch = 512;
    ctx_params.n_threads = 4;
    ctx_params.n_threads_batch = 4;
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(300);
    ctx_params.abort_callback = [](void *data) {
        return std::chrono::steady_clock::now() >=
            *static_cast<std::chrono::steady_clock::time_point *>(data);
    };
    ctx_params.abort_callback_data = &deadline;
    Context state{llama_init_from_model(cached_model, ctx_params), nullptr, nullptr};
    if (!state.ctx) return fail("context creation failed");
    progress("context_ready", started);
    state.batch = llama_batch_ext_init(state.ctx);
    state.sampler = llama_sampler_chain_init(llama_sampler_chain_default_params());
    if (!state.batch || !state.sampler) return fail("sampler creation failed");
    llama_sampler_chain_add(state.sampler, llama_sampler_init_top_k(20));
    llama_sampler_chain_add(state.sampler, llama_sampler_init_top_p(0.8f, 1));
    llama_sampler_chain_add(state.sampler, llama_sampler_init_temp(0.2f));
    llama_sampler_chain_add(state.sampler, llama_sampler_init_dist(42));

    int pos = 0;
    while (pos < n_prompt) {
        const int count = std::min(512, n_prompt - pos);
        if (!set_batch(state.batch, tokens.data() + pos, count, pos) ||
            llama_process(state.ctx, LLAMA_PROCESS_TYPE_DECODE, state.batch))
            return fail(std::chrono::steady_clock::now() >= deadline ? "generation timed out" : "prompt evaluation failed");
        pos += count;
        progress("prefill", started, pos);
    }
    size_t used = 0;
    for (int i = 0; i < max_tokens; ++i) {
        const llama_token token = llama_sampler_sample(state.sampler, state.ctx, -1);
        if (llama_vocab_is_eog(vocab, token)) {
            progress("complete", started, i);
            last_error.clear();
            return static_cast<int>(used);
        }
        char piece[512];
        const int n = llama_token_to_piece(vocab, token, piece, sizeof(piece), 0, true);
        if (n < 0 || used + static_cast<size_t>(n) >= output_capacity)
            return fail("generation output too long");
        std::memcpy(output + used, piece, static_cast<size_t>(n));
        used += static_cast<size_t>(n);
        output[used] = '\0';
        if ((i + 1) % 64 == 0) progress("generating", started, i + 1);
        if (!set_batch(state.batch, &token, 1, pos++) ||
            llama_process(state.ctx, LLAMA_PROCESS_TYPE_DECODE, state.batch))
            return fail(std::chrono::steady_clock::now() >= deadline ? "generation timed out" : "generation failed");
    }
    return fail("generation limit reached");
}

#ifdef DIARY_LLM_STANDALONE
#include <cstdio>
int main(int argc, char **argv) {
    if (argc != 4) return 2;
    std::vector<char> result(65536);
    const int status = diary_llm_generate(argv[1], argv[2], argv[3], 384,
                                          result.data(), result.size());
    if (status < 0) { std::fprintf(stderr, "%s\n", diary_llm_last_error()); return 1; }
    std::printf("%s\n", result.data());
    return 0;
}
#endif
