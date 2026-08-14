<?php

namespace App\Http\Requests;

use Illuminate\Foundation\Http\FormRequest;

class StorePostRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'title' => ['required', 'string', 'max:255'],
            'content' => 'required|min:10',
            'slug' => 'required|unique:posts,slug',
            'address.city' => 'required',
        ];
    }
}
